//! psql's `\encoding` (command.c `exec_command_encoding`): the client
//! encoding, as libpq tracks it. noctua's sessions speak UTF8 — sqlx sets
//! `client_encoding` to it in the startup message, and reads every reply
//! as UTF-8 — so that's what psql's `pset.encoding` would hold, and what
//! a name switches to only when it names UTF8. Names are read as the
//! server reads them (common/encnames.c's `pg_char_to_encoding`).

use crate::types::MetaAction;

/// The client encoding a session speaks, as `pg_encoding_to_char` names
/// it (psql's `:ENCODING`).
pub(super) const CLIENT_ENCODING: &str = "UTF8";

/// `\encoding [name]`: the client encoding; with a name, a switch to it
/// — said nothing when it works, as psql's; one the server wouldn't know
/// refused in psql's words.
pub(super) fn command(args: &[String]) -> MetaAction {
    let Some(name) = args.first() else {
        return MetaAction::Output(vec![CLIENT_ENCODING.into()]);
    };
    match encoding(name) {
        Some(CLIENT_ENCODING) => MetaAction::Output(Vec::new()),
        Some(_) => MetaAction::Fail(vec![
            "\\encoding: a client encoding other than UTF8 isn't supported here yet.".into(),
        ]),
        None => MetaAction::Fail(vec![format!("{name}: invalid encoding name or conversion procedure not found")]),
    }
}

/// The encoding `name` names, as `pg_encoding_to_char` writes it — or
/// none: libpq's `PQsetClientEncoding` reads `auto` as the locale's
/// (noctua's sessions' UTF8), then the server `pg_char_to_encoding`, its
/// letters and digits alone, lower-cased, looked up in its table.
fn encoding(name: &str) -> Option<&'static str> {
    if name == "auto" {
        return Some(CLIENT_ENCODING);
    }
    // NAMEDATALEN: longer is no name in the table.
    if name.is_empty() || name.len() >= 64 {
        return None;
    }
    let key: String = name.chars().filter(char::is_ascii_alphanumeric).map(|c| c.to_ascii_lowercase()).collect();
    ENCODING_NAMES.iter().find(|(alias, _)| *alias == key).map(|(_, encoding)| *encoding)
}

/// encnames.c's `pg_encname_tbl`: each name the server reads, and the
/// encoding it names (`pg_enc2name_tbl`'s spelling).
const ENCODING_NAMES: &[(&str, &str)] = &[
    ("abc", "WIN1258"), ("alt", "WIN866"), ("big5", "BIG5"), ("euccn", "EUC_CN"),
    ("eucjis2004", "EUC_JIS_2004"), ("eucjp", "EUC_JP"), ("euckr", "EUC_KR"), ("euctw", "EUC_TW"),
    ("gb18030", "GB18030"), ("gbk", "GBK"), ("iso88591", "LATIN1"), ("iso885910", "LATIN6"),
    ("iso885913", "LATIN7"), ("iso885914", "LATIN8"), ("iso885915", "LATIN9"), ("iso885916", "LATIN10"),
    ("iso88592", "LATIN2"), ("iso88593", "LATIN3"), ("iso88594", "LATIN4"), ("iso88595", "ISO_8859_5"),
    ("iso88596", "ISO_8859_6"), ("iso88597", "ISO_8859_7"), ("iso88598", "ISO_8859_8"), ("iso88599", "LATIN5"),
    ("johab", "JOHAB"), ("koi8", "KOI8R"), ("koi8r", "KOI8R"), ("koi8u", "KOI8U"),
    ("latin1", "LATIN1"), ("latin10", "LATIN10"), ("latin2", "LATIN2"), ("latin3", "LATIN3"),
    ("latin4", "LATIN4"), ("latin5", "LATIN5"), ("latin6", "LATIN6"), ("latin7", "LATIN7"),
    ("latin8", "LATIN8"), ("latin9", "LATIN9"), ("mskanji", "SJIS"), ("muleinternal", "MULE_INTERNAL"),
    ("shiftjis", "SJIS"), ("shiftjis2004", "SHIFT_JIS_2004"), ("sjis", "SJIS"), ("sqlascii", "SQL_ASCII"),
    ("tcvn", "WIN1258"), ("tcvn5712", "WIN1258"), ("uhc", "UHC"), ("unicode", "UTF8"),
    ("utf8", "UTF8"), ("vscii", "WIN1258"), ("win", "WIN1251"), ("win1250", "WIN1250"),
    ("win1251", "WIN1251"), ("win1252", "WIN1252"), ("win1253", "WIN1253"), ("win1254", "WIN1254"),
    ("win1255", "WIN1255"), ("win1256", "WIN1256"), ("win1257", "WIN1257"), ("win1258", "WIN1258"),
    ("win866", "WIN866"), ("win874", "WIN874"), ("win932", "SJIS"), ("win936", "GBK"),
    ("win949", "UHC"), ("win950", "BIG5"), ("windows1250", "WIN1250"), ("windows1251", "WIN1251"),
    ("windows1252", "WIN1252"), ("windows1253", "WIN1253"), ("windows1254", "WIN1254"), ("windows1255", "WIN1255"),
    ("windows1256", "WIN1256"), ("windows1257", "WIN1257"), ("windows1258", "WIN1258"), ("windows866", "WIN866"),
    ("windows874", "WIN874"), ("windows932", "SJIS"), ("windows936", "GBK"), ("windows949", "UHC"),
    ("windows950", "BIG5"),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn said(args: &[&str]) -> MetaAction {
        command(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn encoding_is_psqls() {
        assert!(matches!(said(&[]), MetaAction::Output(lines) if lines == ["UTF8"]));
        // UTF8 by any name the server reads: said nothing, as psql's.
        for name in ["UTF8", "utf-8", "Unicode", "auto", "u_t_f_8"] {
            assert!(matches!(said(&[name]), MetaAction::Output(lines) if lines.is_empty()), "{name}");
        }
        // Another encoding: a switch this shell can't make.
        for name in ["latin1", "ISO-8859-1", "win1252", "SQL_ASCII"] {
            assert!(matches!(said(&[name]), MetaAction::Fail(lines) if lines[0].contains("other than UTF8")), "{name}");
        }
        // No such name: psql's words.
        assert!(matches!(said(&["bogus"]), MetaAction::Fail(lines)
            if lines == ["bogus: invalid encoding name or conversion procedure not found"]));
        assert!(matches!(said(&[""]), MetaAction::Fail(lines)
            if lines == [": invalid encoding name or conversion procedure not found"]));
        assert!(matches!(said(&["AUTO"]), MetaAction::Fail(_)), "only `auto` is libpq's");
    }

    #[test]
    fn the_table_is_encnames() {
        assert_eq!(ENCODING_NAMES.len(), 81);
        assert!(ENCODING_NAMES.windows(2).all(|w| w[0].0 < w[1].0), "sorted, as the server's binary search needs");
    }
}
