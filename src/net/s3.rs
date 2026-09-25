//! Lists a modification's files in the S3/MinIO bucket that hosts them.
//! Port of `S3StorageHandler`.

use anyhow::{bail, Context, Result};
use quick_xml::events::Event;
use rusty_s3::S3Action;
use std::time::Duration;

use super::CLIENT;
use crate::game::launcher::RemoteFileInfo;
use crate::model::ReposVersion;

/// Fallback credentials for the project's own bucket, as in the C# build.
const DEFAULT_ACCESS_KEY: &str = "S58TYR9ISEZV8PBP8QG1";
const DEFAULT_SECRET_KEY: &str = "b2RU1oqVU5toJRnb4gODrXX8sBSgoLcHRX6qPWxj";

const REGION: &str = "us-east-1";
const SIGNATURE_TTL: Duration = Duration::from_secs(300);

/// Endpoint URLs to try for a manifest's `S3HostLink`, best first.
///
/// The manifests give a bare `host:port` (e.g. `gen.insave.ovh:9000`). The C#
/// build fed that straight to `MinioClient`, which talks **plain HTTP** unless
/// `WithSSL()` is called — and it never was. The project's own MinIO only
/// listens for HTTP on that port, so http has to come first. https is kept as a
/// fallback for any mod that does host its storage behind TLS.
fn endpoint_candidates(host_link: &str) -> Vec<String> {
    // A manifest that already states a scheme is taken at its word.
    if host_link.starts_with("http://") || host_link.starts_with("https://") {
        return vec![host_link.to_owned()];
    }
    vec![format!("http://{host_link}"), format!("https://{host_link}")]
}

/// Every object under the modification's folder, with ETag and size.
pub async fn list_mod_files(version: &ReposVersion) -> Result<Vec<RemoteFileInfo>> {
    if version.s3_host_link.is_empty() || version.s3_bucket_name.is_empty() {
        bail!("modification has no S3 storage configured");
    }

    let mut last_error = None;
    for endpoint in endpoint_candidates(&version.s3_host_link) {
        match list_from_endpoint(version, &endpoint).await {
            Ok(files) => return Ok(files),
            Err(e) => {
                log::warn!("S3 listing via {endpoint} failed: {e:#}");
                last_error = Some(e);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("no usable S3 endpoint")))
}

async fn list_from_endpoint(
    version: &ReposVersion,
    endpoint: &str,
) -> Result<Vec<RemoteFileInfo>> {
    let endpoint = url::Url::parse(endpoint)
        .with_context(|| format!("bad S3 host {}", version.s3_host_link))?;

    let bucket = rusty_s3::Bucket::new(
        endpoint,
        rusty_s3::UrlStyle::Path,
        version.s3_bucket_name.clone(),
        REGION.to_owned(),
    )
    .context("building S3 bucket reference")?;

    let credentials = if version.s3_host_public_key.is_empty()
        || version.s3_host_secret_key.is_empty()
    {
        rusty_s3::Credentials::new(DEFAULT_ACCESS_KEY, DEFAULT_SECRET_KEY)
    } else {
        rusty_s3::Credentials::new(
            version.s3_host_public_key.clone(),
            version.s3_host_secret_key.clone(),
        )
    };

    let prefix = version.s3_folder_name.clone();
    let mut files = Vec::new();
    let mut continuation: Option<String> = None;

    loop {
        let mut action = bucket.list_objects_v2(Some(&credentials));
        {
            let query = action.query_mut();
            if !prefix.is_empty() {
                query.insert("prefix", prefix.clone());
            }
            if let Some(token) = &continuation {
                query.insert("continuation-token", token.clone());
            }
        }
        let signed = action.sign(SIGNATURE_TTL);

        let body = CLIENT
            .get(signed.as_str())
            .timeout(Duration::from_secs(120))
            .send()
            .await
            .context("listing objects in S3 storage")?
            .error_for_status()
            .context("listing objects in S3 storage")?
            .text()
            .await?;

        let page = parse_list_response(&body, &prefix)?;
        files.extend(page.objects);

        match page.next_continuation_token {
            Some(token) if page.is_truncated => continuation = Some(token),
            _ => break,
        }
    }

    Ok(files)
}

/// Direct download URL for one file. The host's port is dropped: the manifests
/// point at a MinIO API port, but plain reads are served on 443.
pub fn file_url(version: &ReposVersion, file_name: &str) -> String {
    let host = version.s3_host_link.split(':').next().unwrap_or(&version.s3_host_link);
    format!(
        "https://{host}/{}/{}/{}",
        version.s3_bucket_name, version.s3_folder_name, file_name
    )
}

struct ListPage {
    objects: Vec<RemoteFileInfo>,
    is_truncated: bool,
    next_continuation_token: Option<String>,
}

fn parse_list_response(xml: &str, prefix: &str) -> Result<ListPage> {
    let mut reader = quick_xml::Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut objects = Vec::new();
    let mut is_truncated = false;
    let mut next_token = None;
    // `rusty-s3` always asks for `encoding-type=url`, so keys come back
    // percent-encoded. Mod files are routinely named `!OFS_ALPHA_art.big`, and
    // saving those as `%21OFS_ALPHA_art.big` would leave the game unable to
    // find them. Only decode when the response says it encoded them.
    let mut keys_are_encoded = false;

    let mut path: Vec<String> = Vec::new();
    let mut key = String::new();
    let mut etag = String::new();
    let mut size: u64 = 0;

    let strip = |key: &str| -> String {
        if prefix.is_empty() {
            key.to_owned()
        } else {
            key.strip_prefix(&format!("{prefix}/")).unwrap_or(key).to_owned()
        }
    };

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                if name == "Contents" {
                    key.clear();
                    etag.clear();
                    size = 0;
                }
                path.push(name);
            }
            Ok(Event::Text(e)) => {
                let text = e.unescape().unwrap_or_default().into_owned();
                let in_contents = path.iter().any(|p| p == "Contents");
                match path.last().map(String::as_str) {
                    Some("Key") if in_contents => key = text,
                    // MinIO quotes ETags, as S3 does.
                    Some("ETag") if in_contents => etag = text.trim_matches('"').to_owned(),
                    Some("Size") if in_contents => size = text.parse().unwrap_or(0),
                    Some("IsTruncated") => is_truncated = text.eq_ignore_ascii_case("true"),
                    // The continuation token is opaque and never encoded.
                    Some("NextContinuationToken") => next_token = Some(text),
                    Some("EncodingType") => keys_are_encoded = text.eq_ignore_ascii_case("url"),
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                if name == "Contents" && !key.is_empty() {
                    // A "directory" marker object has no content of its own.
                    if !key.ends_with('/') {
                        // Keep the raw key; decoding happens after the whole
                        // document is read (see below).
                        objects.push(RemoteFileInfo {
                            file_name: key.clone(),
                            hash: etag.clone(),
                            size,
                        });
                    }
                }
                path.pop();
            }
            Ok(Event::Eof) => break,
            Err(e) => bail!("malformed S3 listing: {e}"),
            _ => {}
        }
    }

    // `EncodingType` is emitted after the `Contents` entries, so the keys can
    // only be decoded once the whole document has been read.
    for object in &mut objects {
        if keys_are_encoded {
            object.file_name = crate::util::percent_decode(&object.file_name);
        }
        object.file_name = strip(&object.file_name);
    }

    Ok(ListPage { objects, is_truncated, next_continuation_token: next_token })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_listing_and_strips_the_prefix() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Name>rotr</Name>
  <Prefix>rotr-individual-files</Prefix>
  <IsTruncated>false</IsTruncated>
  <Contents>
    <Key>rotr-individual-files/ROTR.big</Key>
    <ETag>"D41D8CD98F00B204E9800998ECF8427E"</ETag>
    <Size>1048576</Size>
  </Contents>
  <Contents>
    <Key>rotr-individual-files/Data/INI/GameData.ini</Key>
    <ETag>"0123456789ABCDEF0123456789ABCDEF"</ETag>
    <Size>4096</Size>
  </Contents>
  <Contents>
    <Key>rotr-individual-files/Data/</Key>
    <ETag>""</ETag>
    <Size>0</Size>
  </Contents>
</ListBucketResult>"#;

        let page = parse_list_response(xml, "rotr-individual-files").unwrap();
        assert_eq!(page.objects.len(), 2, "directory markers must be skipped");
        assert_eq!(page.objects[0].file_name, "ROTR.big");
        assert_eq!(page.objects[0].hash, "D41D8CD98F00B204E9800998ECF8427E");
        assert_eq!(page.objects[0].size, 1048576);
        assert_eq!(page.objects[1].file_name, "Data/INI/GameData.ini");
        assert!(!page.is_truncated);
    }

    #[test]
    fn decodes_percent_encoded_keys_when_the_server_says_it_encoded_them() {
        // MinIO emits <EncodingType> *after* the entries, so the parser must
        // not rely on seeing it first.
        let xml = r#"<ListBucketResult>
  <Prefix>ofs-individual-files</Prefix>
  <IsTruncated>false</IsTruncated>
  <Contents>
    <Key>ofs-individual-files/%21%21OFS_ALPHA_Window_16_9.big</Key>
    <ETag>"abc"</ETag>
    <Size>10</Size>
  </Contents>
  <EncodingType>url</EncodingType>
</ListBucketResult>"#;
        let page = parse_list_response(xml, "ofs-individual-files").unwrap();
        assert_eq!(page.objects[0].file_name, "!!OFS_ALPHA_Window_16_9.big");
    }

    #[test]
    fn leaves_keys_alone_when_the_server_did_not_encode_them() {
        // A literal '%' in a name must not be mangled.
        let xml = r#"<ListBucketResult>
  <IsTruncated>false</IsTruncated>
  <Contents>
    <Key>100%25off.big</Key>
    <ETag>"abc"</ETag>
    <Size>10</Size>
  </Contents>
</ListBucketResult>"#;
        let page = parse_list_response(xml, "").unwrap();
        assert_eq!(page.objects[0].file_name, "100%25off.big");
    }

    #[test]
    fn reports_truncation_for_paging() {
        let xml = r#"<ListBucketResult>
  <IsTruncated>true</IsTruncated>
  <NextContinuationToken>abc123</NextContinuationToken>
</ListBucketResult>"#;
        let page = parse_list_response(xml, "").unwrap();
        assert!(page.is_truncated);
        assert_eq!(page.next_continuation_token.as_deref(), Some("abc123"));
    }

    #[test]
    fn tries_plain_http_first_for_a_bare_host_and_port() {
        // MinIO's .NET client defaults to non-SSL, and the project's storage
        // only answers HTTP on the API port; https must not be tried first.
        assert_eq!(
            endpoint_candidates("gen.insave.ovh:9000"),
            vec!["http://gen.insave.ovh:9000", "https://gen.insave.ovh:9000"]
        );
    }

    #[test]
    fn honours_a_scheme_the_manifest_states() {
        assert_eq!(
            endpoint_candidates("https://storage.example/"),
            vec!["https://storage.example/"]
        );
    }

    #[test]
    fn builds_a_download_url_without_the_api_port() {
        let v = ReposVersion {
            s3_host_link: "gen.insave.ovh:9000".into(),
            s3_bucket_name: "rotr".into(),
            s3_folder_name: "rotr-individual-files".into(),
            ..Default::default()
        };
        assert_eq!(
            file_url(&v, "Data/INI/GameData.ini"),
            "https://gen.insave.ovh/rotr/rotr-individual-files/Data/INI/GameData.ini"
        );
    }

    /// Hits the real storage. Run with:
    /// `cargo test --  --ignored live_listing`
    #[test]
    #[ignore = "requires network access to gen.insave.ovh"]
    fn live_listing_reaches_the_project_storage() {
        let version = ReposVersion {
            s3_host_link: "gen.insave.ovh:9000".into(),
            s3_bucket_name: "operationfirestorm".into(),
            s3_folder_name: "ofs-individual-files".into(),
            ..Default::default()
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let files = runtime.block_on(list_mod_files(&version)).expect("listing");

        assert!(!files.is_empty(), "storage returned no files");
        assert!(
            files.iter().all(|f| !f.hash.is_empty() && f.size > 0),
            "every entry needs an ETag and a size"
        );
        // The prefix must be stripped so paths are relative to the mod folder.
        assert!(files.iter().all(|f| !f.file_name.starts_with("ofs-individual-files/")));

        // Names must arrive decoded: this mod ships files like
        // `!!OFS_ALPHA_Window_16_9.big`, and writing `%21%21...` to disk would
        // install a mod the game cannot load.
        assert!(
            files.iter().any(|f| f.file_name.starts_with('!')),
            "expected literal '!' in names, got {:?}",
            files.iter().map(|f| &f.file_name).collect::<Vec<_>>()
        );
        assert!(
            files.iter().all(|f| !f.file_name.contains('%')),
            "percent escapes leaked into file names"
        );

        // And the derived download URL must actually serve bytes.
        let url = file_url(&version, &files[0].file_name);
        let status = runtime
            .block_on(async {
                CLIENT.get(&url).header("Range", "bytes=0-31").send().await
            })
            .expect("range request")
            .status();
        assert!(status.is_success(), "download URL {url} returned {status}");
    }
}

