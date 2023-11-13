use http_from_scratch::app::App;
use std::sync::Arc;

const USAGE: &str = "usage: hfs [--tokio] [--port N] [--root DIR] [--threads N]";

fn main() {
    let mut tokio_mode = false;
    let mut port: u16 = 8080;
    let mut root = String::from("public");
    let mut threads = 8usize;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = |name: &str| args.next().unwrap_or_else(|| die(&format!("{name} needs a value")));
        match a.as_str() {
            "--tokio" => tokio_mode = true,
            "--port" => port = val("--port").parse().unwrap_or_else(|_| die("bad port")),
            "--root" => root = val("--root"),
            "--threads" => threads = val("--threads").parse().unwrap_or_else(|_| die("bad thread count")),
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            _ => die(USAGE),
        }
    }
    let app = Arc::new(App::new(root));
    let addr = format!("127.0.0.1:{port}");
    if tokio_mode {
        let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
        rt.block_on(async {
            let l = tokio::net::TcpListener::bind(&addr).await.unwrap_or_else(|e| die(&e.to_string()));
            eprintln!("hfs (tokio) listening on http://{addr}");
            if let Err(e) = http_from_scratch::server_tokio::serve(l, app).await {
                die(&e.to_string());
            }
        });
    } else {
        let l = std::net::TcpListener::bind(&addr).unwrap_or_else(|e| die(&e.to_string()));
        eprintln!("hfs (std, {threads} threads) listening on http://{addr}");
        if let Err(e) = http_from_scratch::server_std::serve(l, app, threads) {
            die(&e.to_string());
        }
    }
}

fn die(msg: &str) -> ! {
    eprintln!("hfs: {msg}");
    std::process::exit(2)
}
