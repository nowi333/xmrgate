//! The xmrgate server.

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use xmrgate::{rates::Rates, server, Store};

fn setting(flag: &str, env: &str, default: &str) -> String {
    let args: Vec<String> = std::env::args().collect();
    args.iter()
        .position(|arg| arg == flag)
        .and_then(|i| args.get(i + 1).cloned())
        .or_else(|| std::env::var(env).ok())
        .unwrap_or_else(|| default.to_owned())
}

#[tokio::main]
async fn main() {
    if std::env::args().any(|arg| arg == "--help" || arg == "-h") {
        println!(
            "xmrgate {}\n\nAccept Monero without running a node.\n\n\
       Options (or environment variables):\n  \
       --listen <addr>   address to listen on   [XMRGATE_LISTEN, default 127.0.0.1:3080]\n  \
       --data <dir>      where to keep data     [XMRGATE_DATA, default ./xmrgate-data]\n\n\
       Commands:\n  \
       --reset-password  choose a new dashboard password, then exit (stop xmrgate first)",
            env!("CARGO_PKG_VERSION")
        );
        return;
    }

    let listen: SocketAddr = match setting("--listen", "XMRGATE_LISTEN", "127.0.0.1:3080").parse() {
        Ok(addr) => addr,
        Err(_) => fail("the listen address must look like 127.0.0.1:3080"),
    };
    let data = PathBuf::from(setting("--data", "XMRGATE_DATA", "xmrgate-data"));
    if let Err(e) = std::fs::create_dir_all(&data) {
        fail(&format!(
            "cannot create the data directory {}: {e}",
            data.display()
        ));
    }
    let store = match Store::open(&data.join("xmrgate.sqlite")) {
        Ok(store) => Arc::new(store),
        Err(e) => fail(&format!("cannot open the database: {e}")),
    };

    if std::env::args().any(|arg| arg == "--reset-password") {
        eprintln!("New dashboard password (at least 8 characters):");
        let mut password = String::new();
        if std::io::stdin().read_line(&mut password).is_err() {
            fail("could not read the password");
        }
        match server::set_password(&store, password.trim_end_matches(['\r', '\n'])) {
            Ok(()) => println!("Password changed. Start xmrgate again and sign in."),
            Err(e) => fail(&e),
        }
        return;
    }

    let setup_token = std::env::var("XMRGATE_SETUP_TOKEN")
        .ok()
        .filter(|t| !t.is_empty());
    let app = match server::App::new(store, Rates::new(), setup_token, None) {
        Ok(app) => app,
        Err(e) => fail(&format!("cannot start: {e}")),
    };

    let listener = match tokio::net::TcpListener::bind(listen).await {
        Ok(listener) => listener,
        Err(e) => fail(&format!("cannot listen on {listen}: {e}")),
    };
    println!(
        "xmrgate {} listening on http://{listen}",
        env!("CARGO_PKG_VERSION")
    );
    if !app.is_configured() {
        if listen.ip().is_unspecified() {
            // Inside a container or behind a proxy, only the owner knows the public address.
            println!(
                "\nFirst run. To finish the setup, open this page on your gateway's own address:\n"
            );
            println!("  https://YOUR-DOMAIN/setup?code={}\n", app.setup_token());
        } else {
            println!("\nFirst run. Open this address in your browser to finish the setup:\n");
            println!("  http://{listen}/setup?code={}\n", app.setup_token());
        }
    }

    tokio::spawn(app.clone().run());
    let shutdown = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    if let Err(e) = axum::serve(listener, server::router(app))
        .with_graceful_shutdown(shutdown)
        .await
    {
        fail(&format!("server error: {e}"));
    }
}

fn fail(message: &str) -> ! {
    eprintln!("xmrgate: {message}");
    std::process::exit(1)
}
