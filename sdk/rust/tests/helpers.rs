use zcashname_sdk::*;

// Shared with the Dart SDK and zns-mint's request memo vectors.
const UA: &str = "u1d398kq0gfmegkvn0c57zmvq7gcnhxs6g3chfewlxq2yzhdjpx7uk3h80qgku5ygtyr9m7y6swgqe3pqdleu5uvwmangjj8yk7s5j0u78frtw9y9y5lx4c0x3cp054m9nl274xynwf5ad2uah7afyu4wgu3mwg5xvq4zmrdcplt8uqeqqw4vu4kdwngzvsn7gtdwtx3whkwt4z20pr0k";

#[test]
fn mint_memos_match_dart_and_mint_vectors() {
    assert_eq!(
        build_claim_memo("alice", UA, Term::Forever, None).unwrap(),
        format!("ZNS:claim:forever:alice:{UA}")
    );
    assert_eq!(
        build_claim_memo("alice", UA, Term::Years(12), Some("004206")).unwrap(),
        format!("ZNS:claim:004206:12y:alice:{UA}")
    );
    assert_eq!(
        build_claim_memo("alice", UA, Term::Years(1), None).unwrap(),
        format!("ZNS:claim:1y:alice:{UA}")
    );
    assert!(build_claim_memo("alice", UA, Term::Years(99), None).is_ok());
    assert_eq!(
        build_update_memo("alice", UA, None).unwrap(),
        format!("ZNS:update:none:alice:{UA}")
    );
    assert_eq!(
        build_update_memo("alice", UA, Some(Term::Forever)).unwrap(),
        format!("ZNS:update:forever:alice:{UA}")
    );
    assert_eq!(
        build_update_memo("alice", UA, Some(Term::Years(2))).unwrap(),
        format!("ZNS:update:2y:alice:{UA}")
    );
    assert_eq!(
        build_release_memo("alice", UA).unwrap(),
        format!("ZNS:release:alice:{UA}")
    );
    assert!(build_release_memo("alice", TESTNET.treasury_address).is_ok());
}

#[test]
fn rejects_invalid_mint_inputs() {
    for years in [0, 100, 255] {
        assert!(build_claim_memo("alice", UA, Term::Years(years), None).is_err());
        assert!(build_update_memo("alice", UA, Some(Term::Years(years))).is_err());
    }
    for code in ["4206", "a1b2c3", "１２３４５６", "123456\n", ""] {
        assert!(build_claim_memo("alice", UA, Term::Forever, Some(code)).is_err());
        assert!(build_otp_memo(&OtpMemo {
            code: code.into(),
            name: "alice".into(),
            action: OtpAction::Update,
            ua: UA.into(),
            terms: None,
        })
        .is_err());
    }
    for name in ["", "Alice", "alice.zcash", "a-name", "K", &"a".repeat(64)] {
        assert!(build_claim_memo(name, UA, Term::Forever, None).is_err());
        assert!(build_update_memo(name, UA, None).is_err());
        assert!(build_release_memo(name, UA).is_err());
    }
    assert!(build_claim_memo("alice", "t1abc", Term::Forever, None).is_err());
}

#[test]
fn otp_challenges_round_trip_with_memo_padding() {
    for action in [OtpAction::Update, OtpAction::Release, OtpAction::Delist] {
        let memo = OtpMemo {
            code: "004206".into(),
            name: "alice".into(),
            action,
            ua: UA.into(),
            terms: None,
        };
        let text = build_otp_memo(&memo).unwrap();
        assert_eq!(
            text,
            format!("ZNS:otp:004206:alice:{}:{UA}", action.as_str())
        );
        assert_eq!(parse_otp_memo(&text), Some(memo.clone()));
        assert_eq!(
            parse_otp_memo(&format!("{text}{}", "\0".repeat(512 - text.len()))),
            Some(memo)
        );
    }
    for text in [
        "hello",
        &format!("ZNS:otp:123456:alice:claim:{UA}"),
        &format!("ZNS:otp:12345:alice:update:{UA}"),
        &format!("ZNS:otp:123456:Alice:update:{UA}"),
        "ZNS:otp:123456:alice:update:u1bad",
        &format!("ZNS:otp:123456:alice:update:{UA}:extra"),
    ] {
        assert_eq!(parse_otp_memo(text), None);
    }
}

// A mainnet public-key-hash address: where a buyer pays.
const T1: &str = "t1Hsc1LR8yKnbbe3twRp88p6vFfC5t7DLbs";
const AUCTION: Auction = Auction {
    start_zats: 500_000_000,
    min_zats: 150_000_000,
    start_block: 3_600_000,
    end_block: 3_610_000,
};

#[test]
fn marketplace_memos_match_the_mint_grammar() {
    assert_eq!(
        build_list_memo("alice", UA, T1, 250_000_000).unwrap(),
        format!("ZNS:list:alice:{UA}:{T1}:2.5")
    );
    assert_eq!(
        build_list_memo("alice", UA, T1, 1).unwrap(),
        format!("ZNS:list:alice:{UA}:{T1}:0.00000001")
    );
    assert_eq!(
        build_auction_memo("alice", UA, T1, AUCTION).unwrap(),
        format!("ZNS:auction:alice:{UA}:{T1}:5:1.5:3600000:3610000")
    );
    assert_eq!(
        build_delist_memo("alice", UA).unwrap(),
        format!("ZNS:delist:alice:{UA}")
    );
    assert_eq!(
        build_buy_memo("alice", UA).unwrap(),
        format!("ZNS:buy:alice:{UA}")
    );
}

#[test]
fn rejects_listing_terms_the_mint_refuses() {
    assert!(build_list_memo("alice", UA, UA, 1).is_err());
    // A testnet address for a mainnet name.
    assert!(build_list_memo("alice", UA, "tmEZhbWHTpdKMw5it8YDspUXSMGQyFwovpU", 1).is_err());
    assert!(build_list_memo("alice", UA, T1, 21_000_001 * 100_000_000).is_err());
    let ends_at_start = Auction {
        end_block: AUCTION.start_block,
        ..AUCTION
    };
    let flat = Auction {
        min_zats: AUCTION.start_zats,
        ..AUCTION
    };
    assert!(build_auction_memo("alice", UA, T1, ends_at_start).is_err());
    assert!(build_auction_memo("alice", UA, T1, flat).is_err());
}

#[test]
fn listing_challenges_round_trip_with_their_terms() {
    for (action, ask, tail) in [
        (OtpAction::List, Ask::Flat(250_000_000), "2.5"),
        (
            OtpAction::Auction,
            Ask::Dutch(AUCTION),
            "5:1.5:3600000:3610000",
        ),
    ] {
        let memo = OtpMemo {
            code: "417293".into(),
            name: "alice".into(),
            action,
            ua: UA.into(),
            terms: Some(ListingTerms {
                pay_to: T1.into(),
                ask,
            }),
        };
        let text = build_otp_memo(&memo).unwrap();
        assert_eq!(
            text,
            format!("ZNS:otp:417293:alice:{}:{UA}:{T1}:{tail}", action.as_str())
        );
        assert_eq!(parse_otp_memo(&text), Some(memo));
    }
    for text in [
        format!("ZNS:otp:417293:alice:list:{UA}"),
        format!("ZNS:otp:417293:alice:list:{UA}:{T1}:5:1.5:3600000:3610000"),
        format!("ZNS:otp:417293:alice:auction:{UA}:{T1}:1.5:5:3600000:3610000"),
        format!("ZNS:otp:417293:alice:delist:{UA}:{T1}:2.5"),
        format!("ZNS:otp:417293:alice:list:{UA}:{T1}:02.5"),
    ] {
        assert_eq!(parse_otp_memo(&text), None);
    }
    assert!(build_otp_memo(&OtpMemo {
        code: "417293".into(),
        name: "alice".into(),
        action: OtpAction::Update,
        ua: UA.into(),
        terms: Some(ListingTerms {
            pay_to: T1.into(),
            ask: Ask::Flat(1),
        }),
    })
    .is_err());
}

#[test]
fn buy_answers_parse_and_pay_price_plus_code() {
    assert_eq!(
        parse_buy_otp_memo(&format!("ZNS:buyotp:004206:alice:{UA}")),
        Some(BuyOtpMemo {
            code: "004206".into(),
            name: "alice".into(),
            ua: UA.into(),
            dutch: None,
        })
    );
    assert_eq!(
        parse_buy_otp_memo(&format!("ZNS:dutch:004206:alice:{UA}:3.25:1763215200\0\0")),
        Some(BuyOtpMemo {
            code: "004206".into(),
            name: "alice".into(),
            ua: UA.into(),
            dutch: Some(DutchQuote {
                price_zats: 325_000_000,
                until: 1_763_215_200,
            }),
        })
    );
    for text in [
        format!("ZNS:otp:004206:alice:update:{UA}"),
        format!("ZNS:buyotp:004206:alice:{UA}:3.25"),
        format!("ZNS:dutch:004206:alice:{UA}:3.25"),
    ] {
        assert_eq!(parse_buy_otp_memo(&text), None);
    }
    assert_eq!(
        buy_payment_zats(250_000_000, "004206").unwrap(),
        250_004_206
    );
    assert!(buy_payment_zats(250_000_000, "4206").is_err());
}

#[test]
fn unified_address_checksum_and_lengths_are_validated() {
    for address in [UA, TESTNET.treasury_address, MAINNET.treasury_address] {
        assert!(is_valid_unified_address(address));
        assert!(!is_valid_unified_address(&address.to_uppercase()));
        assert!(!is_valid_unified_address(&address[..address.len() - 1]));
        assert!(!is_valid_unified_address(&format!("{address}q")));
        let mut typo = address.as_bytes().to_vec();
        typo[50] = if typo[50] == b'q' { b'p' } else { b'q' };
        assert!(!is_valid_unified_address(
            std::str::from_utf8(&typo).unwrap()
        ));
    }
    for address in [
        "",
        "zs1abc",
        "t1abc",
        &format!("u1{}", "q".repeat(104)),
        &"é".repeat(53),
    ] {
        assert!(!is_valid_unified_address(address));
    }
}

#[test]
fn pricing_matches_dart_protocol_tiers_and_rounds_up() {
    for (name, price) in [
        ("", 12480),
        ("a", 12480),
        ("ab", 2720),
        ("abc", 920),
        ("abcd", 472),
        ("alice", 188),
        ("abcdef", 112),
        ("abcdefg", 48),
        ("abcdefgh", 32),
        ("abcdefghi", 24),
        ("abcdefghij", 18),
        (&"a".repeat(63), 18),
    ] {
        assert_eq!(annual_usd(name), price);
        assert_eq!(forever_usd(name), price * 4);
    }
    assert_eq!(quote_zats(920.0, 1000.0).unwrap(), 920000);
    assert_eq!(quote_zats(18.0, 333.0).unwrap(), 5994);
    assert_eq!(quote_zats(1.0, 0.5).unwrap(), 1);
    assert_eq!(quote_zats(0.0, 1.0).unwrap(), 0);
    for (usd, rate) in [
        (-1.0, 1.0),
        (1.0, -1.0),
        (f64::NAN, 1.0),
        (1.0, f64::INFINITY),
        (f64::MAX, 2.0),
        (u64::MAX as f64, 1.0),
    ] {
        assert!(quote_zats(usd, rate).is_err());
    }
}

#[test]
fn payment_uri_round_trips_a_mint_request() {
    let memo = build_claim_memo("alice", UA, Term::Forever, None).unwrap();
    let uri = build_zip321_uri(TESTNET.treasury_address, Some("0.0123"), Some(&memo)).unwrap();
    assert!(!uri.ends_with('='));
    let parts = parse_zip321_uri(&uri);
    assert_eq!(parts.address, TESTNET.treasury_address);
    assert_eq!(parts.amount, "0.0123");
    assert_eq!(parts.memo_decoded, memo);
    assert_eq!(parts.memo_raw, to_base64_url(&memo));
    assert_eq!(
        build_zip321_uri(UA, None, None).unwrap(),
        format!("zcash:{UA}")
    );
    assert_eq!(
        build_zip321_uri("t1abc", Some("1"), None).unwrap(),
        "zcash:t1abc?amount=1"
    );
}

#[test]
fn payment_amount_boundaries_use_exact_decimal_comparison() {
    for amount in [
        "0",
        "0.00000001",
        "21000000",
        "21000000.00000000",
        "00021000000.00000000",
        "00000.1",
    ] {
        assert!(build_zip321_uri(UA, Some(amount), None).is_ok(), "{amount}");
    }
    for amount in [
        "",
        "-1",
        "1e3",
        "+1",
        "1.",
        ".1",
        "1.123456789",
        "1.2.3",
        "21000001",
        "21000000.00000001",
        "99999999999999999999",
        " 1",
        "１",
    ] {
        assert!(
            build_zip321_uri(UA, Some(amount), None).is_err(),
            "{amount}"
        );
    }
}

#[test]
fn payment_uri_rejects_memo_and_address_errors() {
    assert!(build_zip321_uri(UA, None, Some(&"a".repeat(512))).is_ok());
    assert!(build_zip321_uri(UA, None, Some(&"a".repeat(513))).is_err());
    assert!(build_zip321_uri(UA, None, Some(&"é".repeat(257))).is_err());
    assert!(build_zip321_uri("t1abc", None, Some("hi")).is_err());
    assert!(build_zip321_uri("t1abc", None, Some("")).is_ok());
    for address in [
        "",
        "u1abc?amount=1",
        "zcash:u1abc",
        "u1 abc",
        "u1abc&memo=hi",
        "u1abc#note",
        "u1abc/x",
        "u1abc%3F",
        "é",
    ] {
        assert!(
            build_zip321_uri(address, Some("1"), None).is_err(),
            "{address}"
        );
    }
}

#[test]
fn uri_parser_reads_the_address_query_field() {
    let parts = parse_zip321_uri("zcash:?address=u1abc&amount=1");
    assert_eq!(parts.address, "u1abc");
    assert_eq!(parts.amount, "1");
    assert_eq!(
        parse_zip321_uri("zcash:?address=u1abc&address=u1def").address,
        "u1abc"
    );
    // An address before `?` wins over the query field.
    assert_eq!(
        parse_zip321_uri("zcash:u1abc?address=u1def").address,
        "u1abc"
    );
}

#[test]
fn uri_parser_handles_encoding_vectors() {
    assert_eq!(parse_zip321_uri("ZCASH:u1abc").address, "u1abc");
    assert_eq!(
        parse_zip321_uri("u1abc"),
        Zip321Parts {
            address: "u1abc".into(),
            ..Default::default()
        }
    );
    let parts = parse_zip321_uri("zcash:u1abc?amount=%201.5%20&memo=SGVsbG8%3D");
    assert_eq!(parts.amount, "1.5");
    assert_eq!(parts.memo_decoded, "Hello");
    assert_eq!(
        decode_base64_url(&to_base64_url("Hello é 🌍")),
        "Hello é 🌍"
    );
    assert_eq!(parse_zip321_uri("zcash:u1abc?memo=*").memo_decoded, "");
    assert_eq!(decode_base64_url("_w"), ""); // Invalid UTF-8.
}

#[test]
fn uri_parser_keeps_first_duplicate_payment_fields_like_typescript() {
    let parts = parse_zip321_uri("zcash:u1abc?amount=1&amount=2&memo=SGVsbG8&memo=V29ybGQ");
    assert_eq!(parts.amount, "1");
    assert_eq!(parts.memo_decoded, "Hello");
    assert_eq!(parts.memo_raw, "SGVsbG8");
    let parts = parse_zip321_uri("zcash:u1abc?amount=&amount=2&memo=&memo=SGVsbG8");
    assert_eq!(parts.amount, "");
    assert_eq!(parts.memo_raw, "");
    assert_eq!(parts.memo_decoded, "");
    assert_eq!(
        parse_zip321_uri("zcash:u1abc?%61mount=1&amount=2").amount,
        "1"
    );
}

#[test]
fn uri_parser_preserves_payment_fields_with_malformed_encoding_like_typescript() {
    for query in [
        "amount=1&memo=SGVsbG8&label=%zz",
        "label=%&amount=1&memo=SGVsbG8",
        "%zz=ignored&amount=1&memo=SGVsbG8",
        "label=%FF&amount=1&memo=SGVsbG8",
    ] {
        let parts = parse_zip321_uri(&format!("zcash:u1abc?{query}"));
        assert_eq!(parts.amount, "1", "{query}");
        assert_eq!(parts.memo_raw, "SGVsbG8", "{query}");
        assert_eq!(parts.memo_decoded, "Hello", "{query}");
    }
    let parts = parse_zip321_uri("zcash:u1abc?amount=1&memo=%zz");
    assert_eq!(parts.amount, "1");
    assert_eq!(parts.memo_raw, "%zz");
    assert_eq!(parts.memo_decoded, "");
    assert_eq!(parse_zip321_uri("zcash:u1abc?memo=%").memo_raw, "%");
    assert_eq!(
        parse_zip321_uri("zcash:u1abc?amount=%FF").amount,
        "\u{fffd}"
    );
}
