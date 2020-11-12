use base64;
use crossbeam_channel::{Receiver, TryRecvError, unbounded};
use dns_lookup::lookup_host;
use log::{info};
use std::{str::FromStr, net::{Ipv6Addr, Ipv4Addr, IpAddr}, io::Read};
use std::fs::File;
use std::{sync::{Mutex, Arc, atomic::{AtomicBool, Ordering::SeqCst}}, thread};
use serde_json::Value;

pub enum AppMessage {
    // Name, IsIPV6, port
    Server(String, bool, u16),
    // Username, IpAddress, IpString, Port
    Connect(String, IpAddr, String, u16),
    Disconnect,
    TransferControl(String),
    SetObserver(String, bool),
    LoadAircraft(String),
    Startup,
    Update,
}

fn get_message_str(type_string: &str, data: &str) -> String {
    format!(
        r#"MessageReceived({})"#, 
        serde_json::json!({"type": type_string, "data": data}).to_string()
    )
}

pub struct App {
    app_handle: Arc<Mutex<Option<web_view::Handle<i32>>>>,
    exited: Arc<AtomicBool>,
    rx: Receiver<AppMessage>,
    was_overloaded: bool
}

fn get_ip_from_data(data: &Value) -> Result<IpAddr, String> {
    match data.get("ip") {
        // Parse ip string as Ipv4Addr
        Some(ip_str) => match Ipv4Addr::from_str(ip_str.as_str().unwrap()) {
            Ok(ip) => Ok(IpAddr::V4(ip)),
            Err(_) => match Ipv6Addr::from_str(ip_str.as_str().unwrap()) {
                Ok(ip) => Ok(IpAddr::V6(ip)),
                Err(_) => Err("Invalid IP.".to_string())
            }
        }
        None => match data.get("hostname") {
            // Resolve hostname
            Some(hostname_str) => match lookup_host(hostname_str.as_str().unwrap()) {
                Ok(hostnames) => match hostnames.iter().nth(0) {
                    // Only accept ipv4
                    Some(ip) => Ok(ip.clone()),
                    _ => Err("No valid IP addresses resolved to the specified hostname.".to_string())
                }
                Err(e) => Err(e.to_string())
            }
            None => Err("Invalid hostname.".to_string())
        }
    }
}

impl App {
    pub fn setup() -> Self {
        info!("Creating webview...");
        
        let (tx, rx) = unbounded();

        let mut logo = vec![];
        File::open("assets/logo.png").unwrap().read_to_end(&mut logo).ok();

        let handle = Arc::new(Mutex::new(None));
        let handle_clone = handle.clone();
        let exited = Arc::new(AtomicBool::new(false));
        let exited_clone = exited.clone();
        let dark_theme_class = "bg-dark";

        info!("Spawning webview thread...");

        thread::spawn(move || {
            let webview = web_view::builder()
            .title("Shared Cockpit")
            .content(web_view::Content::Html(format!(r##"<!DOCTYPE html>
                <html>
                <head>
                    <link rel="stylesheet" href="https://stackpath.bootstrapcdn.com/bootstrap/4.5.2/css/bootstrap.min.css" integrity="sha384-JcKb8q3iqJ61gNV9KGb8thSsNjpSL0n8PARn9HuZOnIxN0hoP+VmmDGMN5t9UJ0Z" crossorigin="anonymous">
                    <style>{css}</style>
                </head>
                <body class="{class}">
                <img src="{logo}" class="logo-image"/>
                {body}
                </body>
                <script>
                {js2}
                {js1}
                {js}
                </script>
                </html>
            "##, 
            class = dark_theme_class,
            css = include_str!("../web/stylesheet.css"), 
            js = include_str!("../web/main.js"), 
            js1 = include_str!("../web/list.js"),
            js2 = include_str!("../web/aircraft.js"),
            body = include_str!("../web/index.html"),
            logo = format!("data:image/png;base64,{}", base64::encode(logo.as_slice()))
            )))

            .invoke_handler(move |web_view, arg| {
                let data: serde_json::Value = serde_json::from_str(arg).unwrap();
                match data["type"].as_str().unwrap() {
                    "connect" => {
                        match get_ip_from_data(&data) {
                            Ok(ip) => {
                                tx.send(
                                    AppMessage::Connect(
                                        data["username"].as_str().unwrap().to_string(),
                                        ip, 
                                        if data.get("ip").is_some() {data["ip"].as_str().unwrap().to_string()} else {data["hostname"].as_str().unwrap().to_string()}, 
                                        data["port"].as_u64().unwrap() as u16)
                                    ).ok();
                                },
                            Err(e) => {
                                web_view.eval(
                                    get_message_str("client_fail", e.as_str()).as_str()
                                ).ok();
                            }
                        };
                    },

                    "disconnect" => {tx.send(AppMessage::Disconnect).ok();},

                    "server" => {
                        tx.send(AppMessage::Server(
                            data["username"].as_str().unwrap().to_string(),
                            data["is_v6"].as_bool().unwrap(), 
                            data["port"].as_u64().unwrap() as u16)
                        ).ok();
                    },

                    "transfer_control" => {
                        tx.send(AppMessage::TransferControl(data["target"].as_str().unwrap().to_string())).ok();
                    },

                    "set_observer" => {
                        tx.send(AppMessage::SetObserver(data["target"].as_str().unwrap().to_string(), data["is_observer"].as_bool().unwrap())).ok();
                    }

                    "load_aircraft" => {
                        tx.send(AppMessage::LoadAircraft(data["name"].as_str().unwrap().to_string())).ok();
                    }

                    "startup" => {tx.send(AppMessage::Startup).ok();}

                    "update" => {tx.send(AppMessage::Update).ok();}
                    _ => ()
                };

                Ok(())
            })
            .user_data(0)
            .resizable(true)
            .size(800, 600)
            .build()
            .unwrap();
            
            let mut handle = handle_clone.lock().unwrap();
            *handle = Some(webview.handle());
            std::mem::drop(handle);

            info!("Running webview thread...");

            webview.run().ok();
            exited_clone.store(true, SeqCst);
        });

        // Run
        Self {
            app_handle: handle,
            exited: exited,
            rx,
            was_overloaded: false
        }
    }

    pub fn exited(&self) -> bool {
        return self.exited.load(SeqCst);
    }

    pub fn get_next_message(&self) -> Result<AppMessage, TryRecvError> {
        return self.rx.try_recv();
    }

    pub fn invoke(&self, type_string: &str, data: Option<&str>) {
        let handle = self.app_handle.lock().unwrap();
        if handle.is_none() {return}
        // Send data to javascript
        let data = data.unwrap_or_default().to_string();
        let type_string = type_string.to_owned();
        handle.as_ref().unwrap().dispatch(move |webview| {
            webview.eval(get_message_str(type_string.as_str(), data.as_str()).as_str()).ok();
            Ok(())
        }).ok();
    }

    pub fn error(&self, msg: &str) {
        self.invoke("error", Some(msg));
    }

    pub fn set_port(&self, port: u16) {
        self.invoke("set_port", Some(port.to_string().as_str()));
    }

    pub fn set_ip(&self, ip: &str) {
        self.invoke("set_ip", Some(ip));
    }

    pub fn set_name(&self, name: &str) {
        self.invoke("set_name", Some(name));
    }

    pub fn attempt(&self) {
        self.invoke("attempt", None);
    }

    pub fn connected(&self) {
        self.invoke("connected", None);
    }

    pub fn disconnected(&self) {
        self.invoke("disconnected", None);
    }

    pub fn server_fail(&self, reason: &str) {
        self.invoke("server_fail", Some(reason));
    }

    pub fn client_fail(&self, reason: &str) {
        self.invoke("client_fail", Some(reason));
    }

    pub fn gain_control(&self) {
        self.invoke("control", None);
    }

    pub fn lose_control(&self) {
        self.invoke("lostcontrol", None);
    }

    pub fn server_started(&self, client_count: u16) {
        self.invoke("server", Some(client_count.to_string().as_str()));
    }

    pub fn new_connection(&self, name: &str) {
        self.invoke("newconnection", Some(name));
    }

    pub fn lost_connection(&self, name: &str) {
        self.invoke("lostconnection", Some(name));
    }

    pub fn overloaded(&self) {
        self.invoke("overloaded", None);
    }

    pub fn stable(&self) {
        self.invoke("stable", None);
    }

    pub fn update_overloaded(&self, is_overloaded: bool) {
        if is_overloaded && !self.was_overloaded {
            self.overloaded()
        } else if !is_overloaded && self.was_overloaded {
            self.stable()
        }
    }

    pub fn observing(&self, observing: bool) {
        if observing {
            self.invoke("observing", None);
        } else {
            self.invoke("stop_observing", None);
        }
    }

    pub fn set_observing(&self, name: &str, observing: bool) {
        if observing {
            self.invoke("set_observing", Some(name));
        } else {
            self.invoke("set_not_observing", Some(name));
        }
    }

    pub fn set_incontrol(&self, name: &str) {
        self.invoke("set_incontrol", Some(name));
    }

    pub fn add_aircraft(&self, name: &str) {
        self.invoke("add_aircraft", Some(name));
    }

    pub fn select_config(&self, name: &str) {
        self.invoke("select_active_config", Some(name));
    }

    pub fn version(&self, version: &str) {
        self.invoke("version", Some(version))
    }

    pub fn update_failed(&self) {
        self.invoke("update_failed", None);
    }
}