use crate::{is_valid_name, is_valid_unified_address, Error};

/// A registration term: 1–99 whole years, or forever.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Term {
    Years(u8),
    Forever,
}

impl Term {
    fn field(self) -> Result<String, Error> {
        match self {
            Self::Forever => Ok("forever".into()),
            Self::Years(years @ 1..=99) => Ok(format!("{years}y")),
            Self::Years(_) => Err(Error::InvalidInput("a term is 1–99 years, or forever")),
        }
    }
}

/// Claims are never challenged by OTP.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OtpAction {
    Update,
    Release,
}

impl OtpAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Update => "update",
            Self::Release => "release",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtpMemo {
    pub code: String,
    pub name: String,
    pub action: OtpAction,
    pub ua: String,
}

fn six_digits(code: &str) -> bool {
    code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit())
}

fn check(name: &str, ua: &str) -> Result<(), Error> {
    if !is_valid_name(name) {
        return Err(Error::InvalidInput(
            "a name is 1–63 lowercase letters or digits",
        ));
    }
    if !is_valid_unified_address(ua) {
        return Err(Error::InvalidInput(
            "not a unified address the mint accepts",
        ));
    }
    Ok(())
}

/// Build a claim request, optionally including a protected name's access code.
pub fn build_claim_memo(
    name: &str,
    ua: &str,
    term: Term,
    access_code: Option<&str>,
) -> Result<String, Error> {
    if access_code.is_some_and(|code| !six_digits(code)) {
        return Err(Error::InvalidInput("the access code is six digits"));
    }
    let term = term.field()?;
    check(name, ua)?;
    Ok(match access_code {
        Some(code) => format!("ZNS:claim:{code}:{term}:{name}:{ua}"),
        None => format!("ZNS:claim:{term}:{name}:{ua}"),
    })
}

/// Without a term, an update carries the existing expiry forward.
pub fn build_update_memo(name: &str, ua: &str, term: Option<Term>) -> Result<String, Error> {
    let term = term
        .map(Term::field)
        .transpose()?
        .unwrap_or_else(|| "none".into());
    check(name, ua)?;
    Ok(format!("ZNS:update:{term}:{name}:{ua}"))
}

pub fn build_release_memo(name: &str, ua: &str) -> Result<String, Error> {
    check(name, ua)?;
    Ok(format!("ZNS:release:{name}:{ua}"))
}

/// The mint sends the challenge; the wallet sends the same text back.
pub fn build_otp_memo(memo: &OtpMemo) -> Result<String, Error> {
    if !six_digits(&memo.code) {
        return Err(Error::InvalidInput("the OTP code is six digits"));
    }
    check(&memo.name, &memo.ua)?;
    Ok(format!(
        "ZNS:otp:{}:{}:{}:{}",
        memo.code,
        memo.name,
        memo.action.as_str(),
        memo.ua
    ))
}

/// Parse a challenge, ignoring trailing NUL padding in a 512-byte memo.
/// Invalid or non-OTP text yields `None`.
pub fn parse_otp_memo(text: &str) -> Option<OtpMemo> {
    let parts: Vec<_> = text.trim_end_matches('\0').split(':').collect();
    let ["ZNS", "otp", code, name, action, ua] = parts.as_slice() else {
        return None;
    };
    if !six_digits(code) || check(name, ua).is_err() {
        return None;
    }
    Some(OtpMemo {
        code: (*code).into(),
        name: (*name).into(),
        ua: (*ua).into(),
        action: match *action {
            "update" => OtpAction::Update,
            "release" => OtpAction::Release,
            _ => return None,
        },
    })
}
