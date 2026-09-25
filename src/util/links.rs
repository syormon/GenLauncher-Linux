//! Rewrites share links into direct-download links. Port of `DownloadsLinkParser`.

pub fn parse_download_link(link: &str) -> String {
    let mut link = link.to_owned();

    if link.contains("www.dropbox.com") {
        link = link.replace("?dl=0", "?dl=1");
    }

    if link.contains("https://onedrive.live.com") {
        link = handle_onedrive_link(&link);
    }

    link
}

fn handle_onedrive_link(link: &str) -> String {
    if link.contains("embed") {
        return link.replace("embed", "download");
    }

    let query = link.replace("https://onedrive.live.com/?", "");
    let parts: Vec<&str> = query.split('&').collect();

    let find = |prefix: &str| -> String {
        parts
            .iter()
            .find(|p| p.contains(prefix))
            .map(|p| p.replace(prefix, ""))
            .unwrap_or_default()
    };

    let cid = find("cid=");
    let authkey = find("authkey=");
    // `id=` must not match `cid=`.
    let resid = parts
        .iter()
        .find(|p| p.contains("id=") && !p.contains("cid="))
        .map(|p| p.replace("id=", ""))
        .unwrap_or_default();

    format!("https://onedrive.live.com/download?cid={cid}&resid={resid}&authkey={authkey}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrites_dropbox_and_onedrive() {
        assert_eq!(
            parse_download_link("https://www.dropbox.com/s/x/ROTR.7z?dl=0"),
            "https://www.dropbox.com/s/x/ROTR.7z?dl=1"
        );
        assert_eq!(
            parse_download_link("https://onedrive.live.com/embed?cid=A&resid=B&authkey=C"),
            "https://onedrive.live.com/download?cid=A&resid=B&authkey=C"
        );
    }
}
