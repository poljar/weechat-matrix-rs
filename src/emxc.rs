//! `emxc://` links: the download URL of a file from an encrypted room with
//! the decryption key, IV and hash as query parameters, as the plugin prints
//! them.

use matrix_sdk::ruma::{
    events::room::{EncryptedFile, EncryptedFileHashes, V2EncryptedFileInfo},
    serde::{
        base64::{Standard, UrlSafe},
        Base64,
    },
    OwnedMxcUri,
};
use url::Url;

/// Parse an `emxc://` link back into the encrypted file it describes.
pub(crate) fn parse_emxc(link: &str) -> Result<EncryptedFile, String> {
    let url =
        Url::parse(link).map_err(|error| format!("Invalid link: {error}"))?;

    if url.scheme() != "emxc" {
        return Err("Not an emxc:// link.".to_owned());
    }

    // /_matrix/media/<version>/download/<server>/<media id>
    let segments: Vec<&str> = url
        .path_segments()
        .map(Iterator::collect)
        .unwrap_or_default();
    let (server, media_id) = match segments.as_slice() {
        ["_matrix", "media", _, "download", server, media_id]
            if !server.is_empty() && !media_id.is_empty() =>
        {
            (*server, *media_id)
        }
        _ => {
            return Err("The emxc link is not a media download URL.".to_owned())
        }
    };

    let mut key = None;
    let mut iv = None;
    let mut hash = None;
    for (name, value) in url.query_pairs() {
        match name.as_ref() {
            "key" => key = Some(value.into_owned()),
            "iv" => iv = Some(value.into_owned()),
            "hash" => hash = Some(value.into_owned()),
            _ => (),
        }
    }

    let key: Base64<UrlSafe, [u8; 32]> =
        Base64::parse(key.ok_or("The emxc link has no key.")?)
            .map_err(|error| format!("Bad key in emxc link: {error}"))?;
    let iv: Base64<Standard, [u8; 16]> =
        Base64::parse(iv.ok_or("The emxc link has no iv.")?)
            .map_err(|error| format!("Bad iv in emxc link: {error}"))?;
    let hash: Base64<Standard, [u8; 32]> =
        Base64::parse(hash.ok_or("The emxc link has no hash.")?)
            .map_err(|error| format!("Bad hash in emxc link: {error}"))?;

    Ok(EncryptedFile::new(
        OwnedMxcUri::from(format!("mxc://{server}/{media_id}")),
        V2EncryptedFileInfo::new(key, iv).into(),
        EncryptedFileHashes::with_sha256(hash.into_inner()),
    ))
}

#[cfg(test)]
mod tests {
    use matrix_sdk::ruma::events::room::{
        EncryptedFileHash, EncryptedFileHashAlgorithm, EncryptedFileHashes,
        EncryptedFileInfo, V2EncryptedFileInfo,
    };

    use super::parse_emxc;

    fn link() -> String {
        let info = V2EncryptedFileInfo::encode([1; 32], [2; 16]);
        let hashes = EncryptedFileHashes::with_sha256([3; 32]);
        let Some(EncryptedFileHash::Sha256(hash)) =
            hashes.get(&EncryptedFileHashAlgorithm::Sha256)
        else {
            unreachable!()
        };

        format!(
            "emxc://matrix.org:443/_matrix/media/r0/download/matrix.org/some-media-id?key={}&hash={}&iv={}",
            info.k.encode(),
            hash.encode(),
            info.iv.encode()
        )
    }

    #[test]
    fn emxc_link_round_trips_to_an_encrypted_file() {
        let file = parse_emxc(&link()).unwrap();

        assert_eq!("mxc://matrix.org/some-media-id", file.url.as_str());
        let EncryptedFileInfo::V2(info) = &file.info else {
            panic!("expected v2 info");
        };
        assert_eq!(&[1; 32], info.k.as_bytes());
        assert_eq!(&[2; 16], info.iv.as_bytes());
        match file.hashes.get(&EncryptedFileHashAlgorithm::Sha256) {
            Some(EncryptedFileHash::Sha256(hash)) => {
                assert_eq!(&[3; 32], hash.as_bytes())
            }
            _ => panic!("expected a sha256 hash"),
        }
    }

    #[test]
    fn emxc_parsing_rejects_other_links() {
        assert!(parse_emxc("mxc://matrix.org/some-media-id").is_err());
        assert!(parse_emxc("https://matrix.org/x").is_err());
        assert!(parse_emxc(&link().replace("&iv=", "&xx=")).is_err());
        assert!(parse_emxc(&link().replace("key=", "key=AAAA&k=")).is_err());
    }
}
