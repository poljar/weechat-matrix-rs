use std::{
    cell::RefCell,
    collections::BTreeMap,
    rc::Rc,
    time::{Duration, Instant},
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use matrix_sdk::ruma::OwnedRoomId;
use sha2::{Digest, Sha256};
use weechat::{
    buffer::Buffer,
    hooks::{CommandRun, CommandRunCallback},
    Prefix, ReturnCode, Weechat,
};

use crate::{
    room::{thread_root_from_buffer, RoomHandle},
    Servers,
};

const MAX_UPLOAD_BYTES: usize = 10 * 1024 * 1024;
const MAX_ENCODED_CHUNK_BYTES: usize = 32 * 1024;
const MAX_ACTIVE_UPLOADS: usize = 4;
const UPLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

struct MediaUpload {
    filename: String,
    mime: mime::Mime,
    declared_size: usize,
    expected_sha256: [u8; 32],
    next_chunk: u32,
    data: Vec<u8>,
}

impl MediaUpload {
    fn begin(
        encoded_filename: &str,
        mime: &str,
        declared_size: &str,
        sha256: &str,
    ) -> Result<Self, &'static str> {
        let filename = decode_filename(encoded_filename)?;
        if !valid_filename(&filename) {
            return Err("filename must be a single non-empty file name");
        }

        if mime.len() > 127 || mime.chars().any(char::is_control) {
            return Err("MIME type is invalid or too long");
        }
        let mime: mime::Mime =
            mime.parse().map_err(|_| "MIME type is invalid")?;

        let declared_size = declared_size
            .parse::<usize>()
            .map_err(|_| "declared size must be a decimal byte count")?;

        if declared_size == 0 || declared_size > MAX_UPLOAD_BYTES {
            return Err("declared size is outside the allowed range");
        }

        let expected_sha256 = parse_sha256(sha256)?;

        Ok(Self {
            filename,
            mime,
            declared_size,
            expected_sha256,
            next_chunk: 0,
            data: Vec::with_capacity(declared_size),
        })
    }

    fn append_chunk(
        &mut self,
        chunk_index: &str,
        encoded: &str,
    ) -> Result<(), &'static str> {
        if encoded.len() > MAX_ENCODED_CHUNK_BYTES {
            return Err("encoded chunk exceeds the allowed size");
        }

        let chunk_index = chunk_index
            .parse::<u32>()
            .map_err(|_| "chunk index must be an unsigned integer")?;

        if chunk_index != self.next_chunk {
            return Err("chunk index is out of order");
        }

        let chunk = URL_SAFE_NO_PAD
            .decode(encoded)
            .map_err(|_| "chunk is not valid base64")?;
        let new_len = self
            .data
            .len()
            .checked_add(chunk.len())
            .ok_or("decoded bytes exceed the declared size")?;

        if new_len > self.declared_size {
            return Err("decoded bytes exceed the declared size");
        }

        self.data.extend_from_slice(&chunk);
        self.next_chunk = self
            .next_chunk
            .checked_add(1)
            .ok_or("too many upload chunks")?;
        Ok(())
    }

    fn finish(self) -> Result<(String, mime::Mime, Vec<u8>), &'static str> {
        if self.data.len() != self.declared_size {
            return Err("decoded byte count does not match the declared size");
        }

        if self.mime == mime::IMAGE_PNG && !self.data.starts_with(PNG_SIGNATURE)
        {
            return Err("image/png payload does not have a PNG signature");
        }

        let actual_sha256: [u8; 32] = Sha256::digest(&self.data).into();
        if actual_sha256 != self.expected_sha256 {
            return Err("payload hash does not match the declared SHA-256");
        }

        Ok((self.filename, self.mime, self.data))
    }
}

struct PendingMediaUpload {
    room: RoomHandle,
    room_id: OwnedRoomId,
    thread_root: Option<matrix_sdk::ruma::OwnedEventId>,
    upload: MediaUpload,
    last_activity: Instant,
}

#[derive(Default)]
struct UploadState {
    pending: BTreeMap<String, PendingMediaUpload>,
}

impl UploadState {
    fn prune_expired(&mut self, now: Instant) {
        self.pending.retain(|_, pending| {
            !upload_is_expired(pending.last_activity, now)
        });
    }
}

pub struct MatrixUploadCommand {
    servers: Servers,
    state: Rc<RefCell<UploadState>>,
}

impl MatrixUploadCommand {
    pub fn create(servers: &Servers) -> Result<CommandRun, ()> {
        CommandRun::new(
            "/matrix-upload",
            MatrixUploadCommand {
                servers: servers.clone(),
                state: Rc::new(RefCell::new(UploadState::default())),
            },
        )
    }

    fn report_error(buffer: &Buffer, error: &str) {
        buffer.print(&format!(
            "{}matrix-upload: {}",
            Weechat::prefix(Prefix::Error),
            error
        ));
    }

    fn current_context(
        &self,
        buffer: &Buffer,
    ) -> Result<
        (RoomHandle, Option<matrix_sdk::ruma::OwnedEventId>),
        &'static str,
    > {
        let room = self
            .servers
            .find_room(buffer)
            .ok_or("command must be run in a Matrix room or thread buffer")?;
        Ok((room, thread_root_from_buffer(buffer)))
    }

    fn matches_pending(
        pending: &PendingMediaUpload,
        room: &RoomHandle,
        thread_root: &Option<matrix_sdk::ruma::OwnedEventId>,
    ) -> bool {
        pending.room_id == room.room_id()
            && pending.thread_root.as_ref() == thread_root.as_ref()
    }

    fn begin(&self, buffer: &Buffer, args: &[&str]) {
        if args.len() != 6 {
            Self::report_error(buffer, "usage: /matrix-upload begin <id> <filename-base64url> <mime> <bytes> <sha256>");
            return;
        }

        if !valid_transfer_id(args[1]) {
            Self::report_error(
                buffer,
                "transfer id must be 1-64 URL-safe characters",
            );
            return;
        }

        let Ok((room, thread_root)) = self.current_context(buffer) else {
            Self::report_error(
                buffer,
                "command must be run in a Matrix room or thread buffer",
            );
            return;
        };

        let upload =
            match MediaUpload::begin(args[2], args[3], args[4], args[5]) {
                Ok(upload) => upload,
                Err(error) => {
                    Self::report_error(buffer, error);
                    return;
                }
            };

        let mut state = self.state.borrow_mut();
        state.prune_expired(Instant::now());
        if state.pending.contains_key(args[1]) {
            Self::report_error(buffer, "transfer id is already active");
            return;
        }
        if state.pending.len() >= MAX_ACTIVE_UPLOADS {
            Self::report_error(buffer, "too many active media uploads");
            return;
        }

        state.pending.insert(
            args[1].to_owned(),
            PendingMediaUpload {
                room_id: room.room_id().to_owned(),
                room,
                thread_root,
                upload,
                last_activity: Instant::now(),
            },
        );
    }

    fn chunk(&self, buffer: &Buffer, args: &[&str]) {
        if args.len() != 4 {
            Self::report_error(
                buffer,
                "usage: /matrix-upload chunk <id> <index> <base64url>",
            );
            return;
        }
        if !valid_transfer_id(args[1]) {
            Self::report_error(
                buffer,
                "transfer id must be 1-64 URL-safe characters",
            );
            return;
        }

        let Ok((room, thread_root)) = self.current_context(buffer) else {
            Self::report_error(buffer, "command must be run in the Matrix buffer that began the upload");
            return;
        };

        let mut state = self.state.borrow_mut();
        state.prune_expired(Instant::now());
        let error = {
            let Some(pending) = state.pending.get_mut(args[1]) else {
                Self::report_error(buffer, "no active upload");
                return;
            };

            if !Self::matches_pending(pending, &room, &thread_root) {
                Self::report_error(
                    buffer,
                    "upload buffer or thread does not match transfer id",
                );
                return;
            } else {
                let result = pending.upload.append_chunk(args[2], args[3]);
                if result.is_ok() {
                    pending.last_activity = Instant::now();
                }
                result.err()
            }
        };

        if let Some(error) = error {
            state.pending.remove(args[1]);
            Self::report_error(buffer, error);
        }
    }

    fn commit(&self, buffer: &Buffer, args: &[&str]) {
        if args.len() != 2 {
            Self::report_error(buffer, "usage: /matrix-upload commit <id>");
            return;
        }
        if !valid_transfer_id(args[1]) {
            Self::report_error(
                buffer,
                "transfer id must be 1-64 URL-safe characters",
            );
            return;
        }

        let Ok((room, thread_root)) = self.current_context(buffer) else {
            Self::report_error(buffer, "command must be run in the Matrix buffer that began the upload");
            return;
        };

        let pending = {
            let mut state = self.state.borrow_mut();
            state.prune_expired(Instant::now());
            let Some(pending) = state.pending.get(args[1]) else {
                Self::report_error(buffer, "no active upload");
                return;
            };
            if !Self::matches_pending(pending, &room, &thread_root) {
                Self::report_error(
                    buffer,
                    "upload buffer or thread does not match transfer id",
                );
                return;
            }
            state.pending.remove(args[1])
        };
        let Some(pending) = pending else {
            Self::report_error(buffer, "no active upload");
            return;
        };

        let (filename, mime, data) = match pending.upload.finish() {
            Ok(upload) => upload,
            Err(error) => {
                Self::report_error(buffer, error);
                return;
            }
        };

        let room = pending.room;
        let thread_root = pending.thread_root;
        Weechat::spawn(async move {
            room.send_attachment_bytes(filename, mime, data, thread_root)
                .await
        })
        .detach();
    }

    fn cancel(&self, buffer: &Buffer, args: &[&str]) {
        if args.len() != 2 {
            Self::report_error(buffer, "usage: /matrix-upload cancel <id>");
            return;
        }
        if !valid_transfer_id(args[1]) {
            Self::report_error(
                buffer,
                "transfer id must be 1-64 URL-safe characters",
            );
            return;
        }

        let Ok((room, thread_root)) = self.current_context(buffer) else {
            Self::report_error(buffer, "command must be run in the Matrix buffer that began the upload");
            return;
        };

        let mut state = self.state.borrow_mut();
        state.prune_expired(Instant::now());
        let Some(pending) = state.pending.get(args[1]) else {
            Self::report_error(buffer, "no active upload");
            return;
        };
        if !Self::matches_pending(pending, &room, &thread_root) {
            Self::report_error(
                buffer,
                "upload buffer or thread does not match transfer id",
            );
            return;
        }

        state.pending.remove(args[1]);
    }
}

impl CommandRunCallback for MatrixUploadCommand {
    fn callback(
        &mut self,
        _: &Weechat,
        buffer: &Buffer,
        cmd: std::borrow::Cow<str>,
    ) -> ReturnCode {
        let Some(arguments) = cmd.strip_prefix("/matrix-upload") else {
            return ReturnCode::Ok;
        };
        let args = arguments.split_whitespace().collect::<Vec<_>>();

        match args.first().copied() {
            Some("begin") => self.begin(buffer, &args),
            Some("chunk") => self.chunk(buffer, &args),
            Some("commit") => self.commit(buffer, &args),
            Some("cancel") => self.cancel(buffer, &args),
            _ => Self::report_error(
                buffer,
                "usage: /matrix-upload <begin|chunk|commit|cancel>",
            ),
        }

        // This hook implements the complete command. Letting WeeChat continue
        // would run the same input through normal command dispatch as well and
        // print a misleading "Unknown command" after a handled upload step.
        ReturnCode::OkEat
    }
}

fn valid_filename(filename: &str) -> bool {
    !filename.is_empty()
        && filename.len() <= 255
        && !filename.contains(['/', '\\', '\0'])
        && !filename.chars().any(char::is_control)
}

fn decode_filename(encoded: &str) -> Result<String, &'static str> {
    let bytes = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| "filename must be URL-safe base64 without padding")?;
    String::from_utf8(bytes).map_err(|_| "filename must be valid UTF-8")
}

fn valid_transfer_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
        })
}

fn upload_is_expired(last_activity: Instant, now: Instant) -> bool {
    now.saturating_duration_since(last_activity) >= UPLOAD_IDLE_TIMEOUT
}

fn parse_sha256(value: &str) -> Result<[u8; 32], &'static str> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("SHA-256 must be 64 hexadecimal characters");
    }

    let mut hash = [0_u8; 32];
    for (index, byte) in hash.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| "SHA-256 must be hexadecimal")?;
    }
    Ok(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(bytes: &[u8]) -> Vec<u8> {
        [PNG_SIGNATURE, bytes].concat()
    }

    fn sha256(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn upload_for(bytes: &[u8], filename: &str, mime: &str) -> MediaUpload {
        MediaUpload::begin(
            &URL_SAFE_NO_PAD.encode(filename),
            mime,
            &bytes.len().to_string(),
            &sha256(bytes),
        )
        .unwrap()
    }

    #[test]
    fn rejects_out_of_bounds_begin_metadata() {
        assert!(MediaUpload::begin(
            &URL_SAFE_NO_PAD.encode("image.png"),
            "image/png",
            &(MAX_UPLOAD_BYTES + 1).to_string(),
            &"0".repeat(64),
        )
        .is_err());
        assert!(MediaUpload::begin(
            &URL_SAFE_NO_PAD.encode("../image.png"),
            "image/png",
            "1",
            &"0".repeat(64),
        )
        .is_err());
        assert!(MediaUpload::begin(
            &URL_SAFE_NO_PAD.encode("payload.bin"),
            "not a mime",
            "1",
            &"0".repeat(64),
        )
        .is_err());
    }

    #[test]
    fn rejects_malformed_or_out_of_order_chunks() {
        let bytes = png(b"payload");
        let mut upload = upload_for(&bytes, "image.png", "image/png");
        assert!(upload.append_chunk("1", "AA==").is_err());
        assert!(upload.append_chunk("0", "not-base64").is_err());
        assert!(upload
            .append_chunk("0", &"A".repeat(MAX_ENCODED_CHUNK_BYTES + 1))
            .is_err());
    }

    #[test]
    fn preserves_exact_bytes_across_chunks() {
        let bytes = png(b"split pixels");
        let mut upload = upload_for(&bytes, "image.png", "image/png");
        let split = 10;
        upload
            .append_chunk("0", &URL_SAFE_NO_PAD.encode(&bytes[..split]))
            .unwrap();
        upload
            .append_chunk("1", &URL_SAFE_NO_PAD.encode(&bytes[split..]))
            .unwrap();
        let (_, content_type, actual) = upload.finish().unwrap();
        assert_eq!(content_type, mime::IMAGE_PNG);
        assert_eq!(actual, bytes);
    }

    #[test]
    fn rejects_hash_mismatch_and_wrong_png_signature() {
        let bytes = png(b"payload");
        let mut upload = MediaUpload::begin(
            &URL_SAFE_NO_PAD.encode("image.png"),
            "image/png",
            &bytes.len().to_string(),
            &"0".repeat(64),
        )
        .unwrap();
        upload
            .append_chunk("0", &URL_SAFE_NO_PAD.encode(&bytes))
            .unwrap();
        assert!(upload.finish().is_err());

        let bytes = b"not a png";
        let mut upload = upload_for(bytes, "image.png", "image/png");
        upload
            .append_chunk("0", &URL_SAFE_NO_PAD.encode(bytes))
            .unwrap();
        assert!(upload.finish().is_err());
    }

    #[test]
    fn accepts_pdf_and_octet_stream_without_png_validation() {
        for (filename, mime, bytes) in [
            ("report.pdf", "application/pdf", b"%PDF-1.7\n".as_slice()),
            (
                "archive.bin",
                "application/octet-stream",
                b"\x00\x01\x02".as_slice(),
            ),
        ] {
            let mut upload = upload_for(bytes, filename, mime);
            upload
                .append_chunk("0", &URL_SAFE_NO_PAD.encode(bytes))
                .unwrap();
            let (actual_filename, actual_mime, actual_bytes) =
                upload.finish().unwrap();
            assert_eq!(actual_filename, filename);
            assert_eq!(actual_mime.as_ref(), mime);
            assert_eq!(actual_bytes, bytes);
        }
    }

    #[test]
    fn accepts_only_url_safe_ids_and_filenames() {
        assert!(valid_transfer_id("upload-01_A"));
        assert!(!valid_transfer_id(""));
        assert!(!valid_transfer_id("contains space"));
        assert!(!valid_transfer_id(&"a".repeat(65)));
        assert_eq!(
            decode_filename(&URL_SAFE_NO_PAD.encode("attachment image.png"))
                .unwrap(),
            "attachment image.png"
        );
        assert!(decode_filename("a==").is_err());
    }

    #[test]
    fn expires_abandoned_transfers_after_one_minute() {
        let started = Instant::now();
        assert!(!upload_is_expired(
            started,
            started + UPLOAD_IDLE_TIMEOUT - Duration::from_millis(1)
        ));
        assert!(upload_is_expired(started, started + UPLOAD_IDLE_TIMEOUT));
    }
}
