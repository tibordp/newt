use std::io::Read;

use tauri::http::Request;
use tauri::http::Response;
use tauri::UriSchemeContext;
use url::Url;

pub const SCRIPT: &str = r#"
    window.addEventListener("keydown", (e) => {
        if (e.key == "Escape") {
            window.close();
        } else {
            return;
        }
        e.preventDefault();
    });
"#;

pub fn url_handler(
    _ctx: UriSchemeContext<'_, tauri::Wry>,
    req: Request<Vec<u8>>,
) -> Response<Vec<u8>> {
    let uri = Url::parse(&req.uri().to_string()).unwrap();

    let filename = uri.query_pairs().find(|p| p.0 == "path").unwrap().1;

    let mut file = std::fs::File::open(&*filename).unwrap();
    let metadata = file.metadata().unwrap();

    let mut vec = Vec::with_capacity(metadata.len() as usize);
    file.read_to_end(&mut vec).unwrap();

    let mime_type;
    if filename.ends_with(".png") {
        mime_type = "image/png"
    } else if filename.ends_with(".jpg") {
        mime_type = "image/jpg"
    } else {
        mime_type = "text/plain"
    }

    Response::builder()
        .header("content-type", mime_type)
        .status(200)
        .body(vec)
        .unwrap()
}
