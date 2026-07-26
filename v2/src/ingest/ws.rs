use tokio::net::TcpStream;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream, connect_async, tungstenite::handshake::client::Response,
};

pub async fn connect(
    url: &str,
) -> anyhow::Result<(WebSocketStream<MaybeTlsStream<TcpStream>>, Response)> {
    Ok(connect_async(url).await?)
}
