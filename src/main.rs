#![allow(dead_code)]
#![allow(unused_variables)]

use std::collections::HashMap;
#[cfg(target_os = "linux")]
use libc;

use serde::Deserialize; // import trait for using it
use reqwest;
use std::env::set_current_dir;
use std::fs::{copy, create_dir_all, File, Permissions};
use std::os::unix::fs::{chroot, PermissionsExt};
use std::process::{exit, Command};
use anyhow::{Context, Result};
use bytes::Bytes;
use flate2::read::GzDecoder;
use reqwest::header::{ACCEPT, AUTHORIZATION};
use tar::Archive;
use tempfile::tempdir;

static REGISTRY_URL: &str = "ghcr.io";

// Usage: your_docker.sh run <image> <command> <arg1> <arg2> ...
#[tokio::main]
async fn main() -> Result<()> {

    let args: Vec<_> = std::env::args().collect();

    let image = &args[2];
    let command = &args[3];
    let command_args = &args[4..];

    // 1. Create a temporary directory
    let temp_dir = tempdir()
        .expect("failed to create temporary directory");

    // 2. Copy the command binary into it
    let dest = temp_dir.path().join(command.trim_start_matches("/"));
    create_dir_all(dest.parent().unwrap())
        .expect("failed to create sub directories inside temp directory");

    copy(command, dest.clone())?;

    // 3. chroot the temporary directory
    chroot(temp_dir.path())
        .expect("failed to chroot !");

    // 4. set the current directory to '/' (root)
    set_current_dir("/")
        .expect("failed to change current directory !");

    // 5. Pull the image layers and setup the environment
    pull_image_and_setup_env(image).await;

    // 6. create /dev/null file inside chroot-ed dir 666
    create_dir_all("/dev")
        .expect("failed to create /dev");

    File::create("/dev/null")
            .expect("failed to create /dev/null")
        .set_permissions(Permissions::from_mode(0o666))
            .expect("failed to set permission");

    #[cfg(target_os = "linux")]
    unsafe {
        libc::unshare(libc::CLONE_NEWPID)
    };

    // 7. Execute the binary
    let mut output = Command::new(command)
        .args(command_args)
        .spawn()
        .with_context(|| {
            format!(
                "Tried to run '{}' with arguments {:?}",
                command, command_args
            )
        })?;
    let code = output.wait()?.code().unwrap_or(1);
    drop(temp_dir); // to prevent resource leaking !
    exit(code);

}

#[tokio::test]
async fn test_fetch_rss() {
    pull_image_and_setup_env("alpine").await;
}

async fn pull_image_and_setup_env(image: &str) {

    let image: ImageDetails = image.into();

    // 1. Fetch the realm (URL), service and scope
    let rss = fetch_rss(image.clone()).await;

    // 2. Fetch Bearer token
    let token = fetch_token(rss).await;

    // 3. Ask the hub for layers metadata
    let metadata = fetch_layers_metadata(image.clone(), token.clone()).await;

    // 4. Make sure you are in a virtual env with write permissions on root
    for digest in metadata.get_layers() {
        let data = fetch_blob(image.clone(), digest, token.clone()).await;
        let decoder = GzDecoder::new(data.as_ref());
        let mut archive = Archive::new(decoder);
        archive.unpack("/").unwrap();
    }

}


#[derive(Debug, Deserialize)]
struct LayerMetadata {
    digest: String
}

async fn fetch_blob(image: ImageDetails, digest: String, token: String) -> Bytes {
    let url = format!(
        "https://{}/v2/library/{}/blobs/{}",
        REGISTRY_URL,
        image.name,
        digest,
    );

    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .header(AUTHORIZATION, format!("Bearer {}", token))
        .send()
        .await
        .unwrap();

    let response = response.bytes().await.unwrap();
    return response;
}

#[derive(Debug, Deserialize)]
struct RegistryResponse {
    layers: Vec<LayerMetadata>
}

impl RegistryResponse {
    fn get_layers(&self) -> Vec<String> {
        let layers = &self.layers;
        layers.iter().map(|x| x.digest.to_string()).collect()
    }
}

async fn fetch_layers_metadata(image: ImageDetails, token: String) -> RegistryResponse {
    let url = format!(
        "https://{}/v2/library/{}/manifests/{}",
        REGISTRY_URL,
        image.name,
        image.tag,
    );

    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .header(ACCEPT, "application/vnd.docker.distribution.manifest.v2+json")
        .header(AUTHORIZATION, format!("Bearer {}", token))
        .send()
        .await
        .unwrap();

    let response = response.json::<RegistryResponse>().await.unwrap();
    return response;
}

async fn fetch_token(rss: RSSUnit) -> String {
    let url = format!("{}?service={}&scope={}", rss.realm, rss.service, rss.scope);
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .send()
        .await
        .unwrap();

    let response = response.json::<AuthResponse>().await.unwrap();
    return response.token;
}

#[derive(Deserialize)]
struct AuthResponse {
    token: String,
}

#[derive(Debug)]
struct RSSUnit {
    realm: String,
    service: String,
    scope: String
}

async fn fetch_rss(image: ImageDetails) -> RSSUnit {
    let url = format!(
        "https://{}/v2/library/{}/manifests/{}",
        REGISTRY_URL,
        image.name,
        image.tag
    );

    let client = reqwest::Client::new();

    let response = client
        .get(url)
        .header(ACCEPT, "application/vnd.docker.distribution.manifest.v2+json")
        .send()
        .await
        .unwrap();

    let fields = response.headers().get("www-authenticate")
        .unwrap()
        .to_str()
        .unwrap();

    if let Some((_, rss_fields)) = fields.split_once(" ") {

        let mut rss_populator = HashMap::new();

        rss_fields.split(",")
            .into_iter()
            .for_each(|key_val| {
                let (k, v) = key_val.split_once("=").unwrap();
                // v comes with double quotes added, so we strip them by choosing v[1..len-1]
                rss_populator.insert(k.to_string(), v[1..v.len() - 1].to_string());
            });

        return RSSUnit {
            realm: rss_populator.get("realm").unwrap().to_string(),
            service: rss_populator.get("service").unwrap().to_string(),
            scope: rss_populator.get("scope").unwrap().to_string(),
        };
    }

    panic!("Unexpected response received from docker registry!")
}

#[derive(Clone)]
struct ImageDetails {
    name: String,
    tag: String
}

impl From<&str> for ImageDetails {
    fn from(image: &str) -> Self {
        if image.contains(":") {
            let (name, tag) = image.split_once(":").unwrap();
            return Self {
                name: name.to_string(),
                tag: tag.to_string()
            };
        }
        return Self {
            name: image.to_string(),
            tag: "latest".to_string()
        };
    }
}


