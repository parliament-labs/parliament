use bytes::{Buf, Bytes};

use crate::error::Error;
use crate::io::MySqlBufExt;
use crate::io::ProtocolDecode;
use crate::protocol::response::Status;

/// Indicates successful completion of a previous command sent by the client.
#[derive(Debug)]
pub struct OkPacket {
    pub affected_rows: u64,
    pub last_insert_id: u64,
    pub status: Status,
    pub warnings: u16,
    /// Parliament patch: the human-readable info the server adds —
    /// `Rows matched: 1  Changed: 0  Warnings: 0` — which the mysql CLI
    /// prints. See [`info_string`].
    pub info: String,
}

impl ProtocolDecode<'_> for OkPacket {
    fn decode_with(mut buf: Bytes, _: ()) -> Result<Self, Error> {
        let header = buf.get_u8();
        if header != 0 && header != 0xfe {
            return Err(err_protocol!(
                "expected 0x00 or 0xfe (OK_Packet) but found 0x{:02x}",
                header
            ));
        }

        let affected_rows = buf.get_uint_lenenc();
        let last_insert_id = buf.get_uint_lenenc();
        let status = Status::from_bits_truncate(buf.get_u16_le());
        let warnings = buf.get_u16_le();
        let info = info_string(&buf);

        Ok(Self {
            affected_rows,
            last_insert_id,
            status,
            warnings,
            info,
        })
    }
}

/// Parliament patch: an OK packet's info string, the rest of the packet.
/// The protocol docs say it's raw text without CLIENT_SESSION_TRACK, but
/// MySQL and MariaDB both write it length-encoded (`net_store_data`); a
/// proxy may follow the docs. Read it length-encoded when the length
/// fills the rest exactly, as raw text otherwise — never an error.
fn info_string(rest: &[u8]) -> String {
    let (len, prefix) = match rest.first() {
        None => return String::new(),
        Some(&b) if b < 0xfb => (u64::from(b), 1),
        Some(0xfc) if rest.len() >= 3 => (u64::from(u16::from_le_bytes([rest[1], rest[2]])), 3),
        Some(0xfd) if rest.len() >= 4 => (u64::from_le_bytes([rest[1], rest[2], rest[3], 0, 0, 0, 0, 0]), 4),
        _ => return String::from_utf8_lossy(rest).into_owned(),
    };
    let text = match usize::try_from(len) {
        Ok(len) if prefix + len == rest.len() => &rest[prefix..],
        _ => rest,
    };
    String::from_utf8_lossy(text).into_owned()
}

#[test]
fn test_decode_ok_packet() {
    const DATA: &[u8] = b"\x00\x00\x00\x02@\x00\x00";

    let p = OkPacket::decode(DATA.into()).unwrap();

    assert_eq!(p.affected_rows, 0);
    assert_eq!(p.last_insert_id, 0);
    assert_eq!(p.warnings, 0);
    assert_eq!(p.info, "");
    assert!(p.status.contains(Status::SERVER_STATUS_AUTOCOMMIT));
    assert!(p.status.contains(Status::SERVER_SESSION_STATE_CHANGED));
}

#[test]
fn test_decode_ok_packet_with_info() {
    const DATA: &[u8] = b"\x00\x00\x00\x01\x00\x00\x00(Rows matched: 1  Changed: 0  Warnings: 0";

    let p = OkPacket::decode(DATA.into()).unwrap();

    assert_eq!(p.info, "Rows matched: 1  Changed: 0  Warnings: 0");
    assert!(p.status.contains(Status::SERVER_STATUS_IN_TRANS));

    // Raw text, as the protocol docs describe it, reads too.
    const RAW: &[u8] = b"\x00\x00\x00\x01\x00\x00\x00Rows matched: 1  Changed: 0  Warnings: 0";
    assert_eq!(OkPacket::decode(RAW.into()).unwrap().info, "Rows matched: 1  Changed: 0  Warnings: 0");
}
