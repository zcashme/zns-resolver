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
    List,
    Auction,
    Delist,
}

impl OtpAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Update => "update",
            Self::Release => "release",
            Self::List => "list",
            Self::Auction => "auction",
            Self::Delist => "delist",
        }
    }
}

/// An auction's price falls from `start_zats` at `start_block` to
/// `min_zats` at `end_block`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Auction {
    pub start_zats: u64,
    pub min_zats: u64,
    pub start_block: u32,
    pub end_block: u32,
}

/// What a buyer pays, in zatoshis.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    Flat(u64),
    Dutch(Auction),
}

/// A listing's terms: the transparent address the buyer pays, and the ask.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListingTerms {
    pub pay_to: String,
    pub ask: Ask,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OtpMemo {
    pub code: String,
    pub name: String,
    pub action: OtpAction,
    pub ua: String,
    /// Present exactly when `action` is `List` (a flat ask) or `Auction` (a dutch ask).
    pub terms: Option<ListingTerms>,
}

/// The mint's answer to a buy. `dutch` is set for an auction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuyOtpMemo {
    pub code: String,
    pub name: String,
    pub ua: String,
    pub dutch: Option<DutchQuote>,
}

/// The price the buyer's block fixed, and the unix time the quote holds until.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DutchQuote {
    pub price_zats: u64,
    pub until: u64,
}

const ZATS_PER_ZEC: u64 = 100_000_000;
const MAX_ZATS: u64 = crate::zip321::MAX_ZEC * ZATS_PER_ZEC;

fn six_digits(code: &str) -> bool {
    code.len() == 6 && code.bytes().all(|b| b.is_ascii_digit())
}

/// Decimal digits with no leading zero.
fn decimal(text: &str) -> bool {
    !text.is_empty()
        && text.bytes().all(|b| b.is_ascii_digit())
        && (text.len() == 1 || !text.starts_with('0'))
}

/// Zatoshis as the mint writes ZEC: whole zatoshis stay an integer, and a
/// fraction drops trailing zeros (`5`, `1.5`, `0.00000001`).
fn encode_zec(zats: u64) -> Result<String, Error> {
    if zats > MAX_ZATS {
        return Err(Error::InvalidInput(
            "an amount is whole zatoshis within the money supply",
        ));
    }
    let (whole, frac) = (zats / ZATS_PER_ZEC, zats % ZATS_PER_ZEC);
    if frac == 0 {
        return Ok(whole.to_string());
    }
    Ok(format!(
        "{whole}.{}",
        format!("{frac:08}").trim_end_matches('0')
    ))
}

/// ZEC text as the mint reads it, in zatoshis.
fn decode_zec(text: &str) -> Option<u64> {
    let (whole, frac) = text.split_once('.').unwrap_or((text, ""));
    if !decimal(whole) || frac.len() > 8 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if text.contains('.') && frac.is_empty() {
        return None;
    }
    let frac: u64 = format!("{frac:0<8}").parse().ok()?;
    let zats = whole
        .parse::<u64>()
        .ok()?
        .checked_mul(ZATS_PER_ZEC)?
        .checked_add(frac)?;
    (zats <= MAX_ZATS).then_some(zats)
}

fn decode_height(text: &str) -> Option<u32> {
    decimal(text).then(|| text.parse().ok()).flatten()
}

/// The shape of a transparent public-key-hash address on the UA's network:
/// `t1` on mainnet, `tm` on testnet, 35 base58 characters. The checksum is
/// not verified; the mint refuses a bad one and keeps the listing fee.
fn is_pay_to(address: &str, ua: &str) -> bool {
    const BASE58: &[u8] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    let prefix = if ua.starts_with("utest1") { "tm" } else { "t1" };
    address.len() == 35
        && address.starts_with(prefix)
        && address.bytes().all(|b| BASE58.contains(&b))
}

/// `<t1>:<price>`, or `<t1>:<start>:<min>:<start block>:<end block>`.
fn terms_fields(terms: &ListingTerms, ua: &str) -> Result<String, Error> {
    if !is_pay_to(&terms.pay_to, ua) {
        return Err(Error::InvalidInput(
            "pay to a transparent t1 address on the name's network",
        ));
    }
    match terms.ask {
        Ask::Flat(price) => Ok(format!("{}:{}", terms.pay_to, encode_zec(price)?)),
        Ask::Dutch(auction) => {
            if auction.end_block <= auction.start_block || auction.start_zats <= auction.min_zats {
                return Err(Error::InvalidInput(
                    "an auction ends after it starts, and its price falls",
                ));
            }
            Ok(format!(
                "{}:{}:{}:{}:{}",
                terms.pay_to,
                encode_zec(auction.start_zats)?,
                encode_zec(auction.min_zats)?,
                auction.start_block,
                auction.end_block
            ))
        }
    }
}

/// Reads `<t1>:<terms>` back. `fields` are the parts after the UA.
fn parse_terms_fields(fields: &[&str], action: OtpAction, ua: &str) -> Option<ListingTerms> {
    let (pay_to, rest) = fields.split_first()?;
    if !is_pay_to(pay_to, ua) {
        return None;
    }
    let ask = match (action, rest) {
        (OtpAction::List, [price]) => Ask::Flat(decode_zec(price)?),
        (OtpAction::Auction, [start, min, start_block, end_block]) => {
            let auction = Auction {
                start_zats: decode_zec(start)?,
                min_zats: decode_zec(min)?,
                start_block: decode_height(start_block)?,
                end_block: decode_height(end_block)?,
            };
            if auction.end_block <= auction.start_block || auction.start_zats <= auction.min_zats {
                return None;
            }
            Ask::Dutch(auction)
        }
        _ => return None,
    };
    Some(ListingTerms {
        pay_to: (*pay_to).into(),
        ask,
    })
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

/// List at a price: `ZNS:list:<name>:<ua>:<t1>:<price>`. `ua` is the name's
/// current address; a buyer pays `price_zats` to `pay_to`.
pub fn build_list_memo(
    name: &str,
    ua: &str,
    pay_to: &str,
    price_zats: u64,
) -> Result<String, Error> {
    check(name, ua)?;
    let terms = ListingTerms {
        pay_to: pay_to.into(),
        ask: Ask::Flat(price_zats),
    };
    Ok(format!(
        "ZNS:list:{name}:{ua}:{}",
        terms_fields(&terms, ua)?
    ))
}

/// List as an auction:
/// `ZNS:auction:<name>:<ua>:<t1>:<start>:<min>:<start block>:<end block>`.
pub fn build_auction_memo(
    name: &str,
    ua: &str,
    pay_to: &str,
    auction: Auction,
) -> Result<String, Error> {
    check(name, ua)?;
    let terms = ListingTerms {
        pay_to: pay_to.into(),
        ask: Ask::Dutch(auction),
    };
    Ok(format!(
        "ZNS:auction:{name}:{ua}:{}",
        terms_fields(&terms, ua)?
    ))
}

pub fn build_delist_memo(name: &str, ua: &str) -> Result<String, Error> {
    check(name, ua)?;
    Ok(format!("ZNS:delist:{name}:{ua}"))
}

/// Buy a listed name: `ZNS:buy:<name>:<ua>`. `ua` is the buyer's address,
/// the one the sale binds the name to.
pub fn build_buy_memo(name: &str, ua: &str) -> Result<String, Error> {
    check(name, ua)?;
    Ok(format!("ZNS:buy:{name}:{ua}"))
}

/// The mint sends the challenge; the wallet sends the same text back. A list
/// or an auction challenge carries its terms after the UA.
pub fn build_otp_memo(memo: &OtpMemo) -> Result<String, Error> {
    if !six_digits(&memo.code) {
        return Err(Error::InvalidInput("the OTP code is six digits"));
    }
    check(&memo.name, &memo.ua)?;
    let head = format!(
        "ZNS:otp:{}:{}:{}:{}",
        memo.code,
        memo.name,
        memo.action.as_str(),
        memo.ua
    );
    match (memo.action, &memo.terms) {
        (OtpAction::Update | OtpAction::Release | OtpAction::Delist, None) => Ok(head),
        (
            OtpAction::List,
            Some(
                terms @ ListingTerms {
                    ask: Ask::Flat(_), ..
                },
            ),
        )
        | (
            OtpAction::Auction,
            Some(
                terms @ ListingTerms {
                    ask: Ask::Dutch(_), ..
                },
            ),
        ) => Ok(format!("{head}:{}", terms_fields(terms, &memo.ua)?)),
        _ => Err(Error::InvalidInput(
            "a list or auction challenge carries its terms, and no other does",
        )),
    }
}

/// Parse a challenge, ignoring trailing NUL padding in a 512-byte memo.
/// Invalid or non-OTP text yields `None`.
pub fn parse_otp_memo(text: &str) -> Option<OtpMemo> {
    let parts: Vec<_> = text.trim_end_matches('\0').split(':').collect();
    let ["ZNS", "otp", code, name, action, ua, rest @ ..] = parts.as_slice() else {
        return None;
    };
    if !six_digits(code) || check(name, ua).is_err() {
        return None;
    }
    let action = match *action {
        "update" => OtpAction::Update,
        "release" => OtpAction::Release,
        "list" => OtpAction::List,
        "auction" => OtpAction::Auction,
        "delist" => OtpAction::Delist,
        _ => return None,
    };
    let terms = match action {
        OtpAction::List | OtpAction::Auction => Some(parse_terms_fields(rest, action, ua)?),
        _ if rest.is_empty() => None,
        _ => return None,
    };
    Some(OtpMemo {
        code: (*code).into(),
        name: (*name).into(),
        action,
        ua: (*ua).into(),
        terms,
    })
}

/// Parse the mint's answer to a buy: `ZNS:buyotp:<code>:<name>:<ua>`, or
/// `ZNS:dutch:<code>:<name>:<ua>:<price>:<until>` for an auction. Trailing
/// NUL padding is ignored; anything else yields `None`.
pub fn parse_buy_otp_memo(text: &str) -> Option<BuyOtpMemo> {
    let parts: Vec<_> = text.trim_end_matches('\0').split(':').collect();
    let (code, name, ua, dutch) = match parts.as_slice() {
        ["ZNS", "buyotp", code, name, ua] => (code, name, ua, None),
        ["ZNS", "dutch", code, name, ua, price, until] => (
            code,
            name,
            ua,
            Some(DutchQuote {
                price_zats: decode_zec(price)?,
                until: decimal(until).then(|| until.parse().ok()).flatten()?,
            }),
        ),
        _ => return None,
    };
    if !six_digits(code) || check(name, ua).is_err() {
        return None;
    }
    Some(BuyOtpMemo {
        code: (*code).into(),
        name: (*name).into(),
        ua: (*ua).into(),
        dutch,
    })
}

/// What a buyer pays to the listing's t1: the price plus the buy code, the
/// code read as zatoshis (`004206` adds 4,206).
pub fn buy_payment_zats(price_zats: u64, code: &str) -> Result<u64, Error> {
    if !six_digits(code) {
        return Err(Error::InvalidInput("the buy code is six digits"));
    }
    let code: u64 = code.parse().expect("six digits fit u64");
    price_zats
        .checked_add(code)
        .filter(|zats| *zats <= MAX_ZATS)
        .ok_or(Error::InvalidInput(
            "an amount is whole zatoshis within the money supply",
        ))
}
