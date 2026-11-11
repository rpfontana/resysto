use std::error::Error;

use axum::Router;
use tower_http::services::{ServeDir, ServeFile};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let addr = std::env::var("RESYSTO_ADDR").unwrap_or_else(|_| "127.0.0.1:3000".into());
    let site = std::env::var("RESYSTO_SITE").unwrap_or_else(|_| "web/build".into());

    let spa = ServeDir::new(&site).fallback(ServeFile::new(format!("{site}/200.html")));
    let app = Router::new().fallback_service(spa);

    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("listening on http://{addr}");
    axum::serve(listener, app).await?;
    Ok(())
}
