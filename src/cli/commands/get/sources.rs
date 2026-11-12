//! The sources `get` fetches prices from when the journal has no
//! `prices/getprices` helper: each one's request, and the reading of its
//! response, a pure function of the response and the time.

use std::collections::{BTreeMap, BTreeSet};

use crate::dates::{Day, parsedate};
use crate::decimal::Decimal;

/// A source of prices, as a three-word `price:` tag names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Yahoo,
    Ecb,
}

impl Source {
    pub const ALL: [Source; 2] = [Source::Yahoo, Source::Ecb];

    pub fn name(self) -> &'static str {
        match self {
            Source::Yahoo => "yahoo",
            Source::Ecb => "ecb",
        }
    }

    /// The source with this name.
    pub fn parse(name: &str) -> Option<Source> {
        Source::ALL.into_iter().find(|s| s.name() == name)
    }

    /// The last day whose price is final: for Yahoo yesterday, a day's close
    /// being final only when the day is over; for ECB today, a reference
    /// rate being final when it is published.
    pub fn last_day(self, today: Day) -> Day {
        match self {
            Source::Yahoo => today.add_days(-1),
            Source::Ecb => today,
        }
    }

    /// Whether the source can have this ticker, or else what one looks
    /// like: any for Yahoo, a pair of currency codes for ECB.
    pub fn check_ticker(self, ticker: &str) -> Result<(), &'static str> {
        match self {
            Source::Yahoo => Ok(()),
            Source::Ecb => match ecb_pair(ticker) {
                Some(_) => Ok(()),
                None => Err("two different currency codes joined by -, such as USD-CAD"),
            },
        }
    }

    /// A ticker's prices on the days from `start` to `end` that are final, in
    /// one request; None if the source does not know the ticker. `now` is
    /// the time in seconds since 1970.
    pub fn fetch(
        self,
        agent: &ureq::Agent,
        ticker: &str,
        start: Day,
        end: Day,
        now: i64,
    ) -> Result<Option<Series>, String> {
        match self {
            Source::Yahoo => yahoo(agent, YAHOO, ticker, start, end, now),
            Source::Ecb => ecb(agent, ECB, ticker, start, end),
        }
    }
}

/// A ticker's daily closing prices, with the currency the source quotes it
/// in and the source's name for it.
#[derive(Debug, PartialEq)]
pub struct Series {
    pub currency: String,
    pub name: String,
    pub closes: BTreeMap<Day, Decimal>,
}

const YAHOO: &str = "https://query1.finance.yahoo.com";

const ECB: &str = "https://data-api.ecb.europa.eu";

/// The User-Agent sent: Yahoo refuses some clients' default ones.
const USER_AGENT: &str = concat!("hledgers/", env!("CARGO_PKG_VERSION"));

fn epoch() -> Day {
    Day::from_ymd(1970, 1, 1).expect("a valid date")
}

/// A ticker in a URL path: letters, digits and `-._~=` as they are, other
/// bytes percent-encoded (`^GSPC` is `%5EGSPC`).
fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~=".contains(&b) {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// The chart request for a ticker's daily bars from `start` to `end`, with a
/// day more on each side, since Yahoo stamps some bars before midnight UTC.
fn yahoo_url(base: &str, ticker: &str, start: Day, end: Day) -> String {
    let secs = |d: Day| d.diff_days(epoch()) * 86400;
    format!(
        "{base}/v8/finance/chart/{}?period1={}&period2={}&interval=1d",
        encode(ticker),
        secs(start.add_days(-1)),
        secs(end.add_days(2))
    )
}

/// Yahoo's closes for a ticker from the server at `base`, on the days from
/// `start` to `end`; None if it answers 404 or does not know the ticker.
fn yahoo(
    agent: &ureq::Agent,
    base: &str,
    ticker: &str,
    start: Day,
    end: Day,
    now: i64,
) -> Result<Option<Series>, String> {
    let request = agent
        .get(yahoo_url(base, ticker, start, end))
        .header("User-Agent", USER_AGENT);
    let body = match request.call() {
        Ok(mut r) => r.body_mut().read_to_string().map_err(|e| e.to_string())?,
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let mut series = parse_yahoo(&body, now)?;
    if let Some(s) = &mut series {
        s.closes.retain(|d, _| (start..=end).contains(d));
    }
    Ok(series)
}

/// A Yahoo chart response read: the ticker's currency, name and closes by
/// day, or None if it has no currency, as for a ticker Yahoo does not know.
/// A bar's day is its time at the exchange plus an hour, since `gmtoffset` is
/// the exchange's offset now and FX bars are stamped at its midnight, an
/// hour earlier in summer. Null closes are left out, and so are the bars of
/// the exchange's today, which is not over. Closes are single-precision
/// numbers, each written as the shortest decimal that reads as the same one.
pub fn parse_yahoo(body: &str, now: i64) -> Result<Option<Series>, String> {
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("unreadable response from Yahoo: {e}"))?;
    let r = &v["chart"]["result"][0];
    let meta = &r["meta"];
    let Some(currency) = meta["currency"].as_str() else {
        return Ok(None);
    };
    let offset = meta["gmtoffset"].as_i64().unwrap_or(0);
    let today = (now + offset).div_euclid(86400);
    let name = meta["longName"].as_str().or(meta["shortName"].as_str());
    let times = r["timestamp"].as_array().map_or(&[][..], Vec::as_slice);
    let closes = r["indicators"]["quote"][0]["close"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    let mut out = BTreeMap::new();
    for (t, c) in times.iter().zip(closes) {
        let (Some(t), Some(c)) = (t.as_i64(), c.as_f64()) else {
            continue;
        };
        let day = (t + offset + 3600).div_euclid(86400);
        if day < today {
            out.insert(epoch().add_days(day), (c as f32).to_string().parse()?);
        }
    }
    Ok(Some(Series {
        currency: currency.to_string(),
        name: name.unwrap_or("").to_string(),
        closes: out,
    }))
}

/// The currencies of an ECB pair, `A-B`, whose price is that of 1 A in B:
/// two different codes of three letters, uppercased, as ECB has them.
fn ecb_pair(ticker: &str) -> Option<(String, String)> {
    let (a, b) = ticker.split_once('-')?;
    let code = |c: &str| {
        (c.len() == 3 && c.bytes().all(|b| b.is_ascii_alphabetic())).then(|| c.to_ascii_uppercase())
    };
    let (a, b) = (code(a)?, code(b)?);
    (a != b).then_some((a, b))
}

/// The request for the euro reference rates of a pair's currencies other
/// than the euro, on the days from `start` to `end`.
fn ecb_url(base: &str, a: &str, b: &str, start: Day, end: Day) -> String {
    let codes: Vec<&str> = [a, b].into_iter().filter(|c| *c != "EUR").collect();
    format!(
        "{base}/service/data/EXR/D.{}.EUR.SP00.A?startPeriod={start}&endPeriod={end}&format=jsondata&detail=dataonly",
        codes.join("+")
    )
}

/// ECB's prices for a pair from the server at `base`, on the days from
/// `start` to `end`; None if it answers 404, as for a currency it does not
/// publish.
fn ecb(
    agent: &ureq::Agent,
    base: &str,
    ticker: &str,
    start: Day,
    end: Day,
) -> Result<Option<Series>, String> {
    let (a, b) = ecb_pair(ticker).ok_or("not two currency codes joined by -")?;
    let request = agent
        .get(ecb_url(base, &a, &b, start, end))
        .header("User-Agent", USER_AGENT);
    let body = match request.call() {
        Ok(mut r) => r.body_mut().read_to_string().map_err(|e| e.to_string())?,
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    parse_ecb(&body, &a, &b)
}

/// An ECB response for the pair `A-B` read: the price of 1 A in B by day.
/// ECB's rates are units of a currency for 1 euro. With the euro as A the
/// price is B's rate, as published; otherwise it is B's rate over A's (the
/// euro's being 1) on the days both have one, rounded to six significant
/// digits. The series of a response are in the order of its currency
/// dimension, and its days are in its observation dimension; null rates are
/// left out. An empty body, ECB's answer for days without rates, is no
/// prices; None if the response has rates but none for a currency of the
/// pair, as when ECB does not publish it.
pub fn parse_ecb(body: &str, a: &str, b: &str) -> Result<Option<Series>, String> {
    let mut series = Series {
        currency: b.to_string(),
        name: format!("{a}-{b}"),
        closes: BTreeMap::new(),
    };
    if body.is_empty() {
        return Ok(Some(series));
    }
    let v: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("unreadable response from ECB: {e}"))?;
    let ids = |dimension: &serde_json::Value| -> Vec<String> {
        let values = (dimension["values"].as_array()).map_or(&[][..], Vec::as_slice);
        let id = |v: &serde_json::Value| v["id"].as_str().unwrap_or("").to_string();
        values.iter().map(id).collect()
    };
    let dimensions = &v["structure"]["dimensions"];
    let currencies = ids(&dimensions["series"][1]);
    let days: Vec<Option<Day>> = (ids(&dimensions["observation"][0]).iter())
        .map(|d| parsedate(d))
        .collect();
    // each currency's rates by day, a rate of zero, which no currency has,
    // left out rather than divided by
    let mut rates: BTreeMap<&str, BTreeMap<Day, Decimal>> = BTreeMap::new();
    for (key, s) in v["dataSets"][0]["series"].as_object().into_iter().flatten() {
        let index = key.split(':').nth(1).and_then(|i| i.parse::<usize>().ok());
        let Some(currency) = index.and_then(|i| currencies.get(i)) else {
            continue;
        };
        let r = rates.entry(currency.as_str()).or_default();
        for (k, o) in s["observations"].as_object().into_iter().flatten() {
            let day = k.parse::<usize>().ok().and_then(|k| *days.get(k)?);
            let rate = (o[0].as_number()).and_then(|n| n.to_string().parse::<Decimal>().ok());
            if let (Some(d), Some(rate)) = (day, rate.filter(|r| !r.is_zero())) {
                r.insert(d, rate);
            }
        }
    }
    let missing = |c: &str| c != "EUR" && !rates.contains_key(c);
    if missing(a) || missing(b) {
        return Ok(None);
    }
    let one = Decimal::from_i64(1);
    let rate = |c: &str, d: &Day| {
        if c == "EUR" {
            Some(&one)
        } else {
            rates.get(c)?.get(d)
        }
    };
    let all: BTreeSet<&Day> = rates.values().flat_map(BTreeMap::keys).collect();
    for d in all {
        if let (Some(ra), Some(rb)) = (rate(a, d), rate(b, d)) {
            let price = if a == "EUR" {
                rb.clone()
            } else {
                six_digits(&(rb / ra))
            };
            series.closes.insert(*d, price);
        }
    }
    Ok(Some(series))
}

/// A positive number rounded half to even to six significant digits, with
/// the fewest decimal places that show it; a whole number from a million up.
fn six_digits(x: &Decimal) -> Decimal {
    let whole = x.digits_count() as i64 - i64::from(x.places());
    x.round_to((6 - whole).clamp(0, 255) as u8).normalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::http::serve_once;

    fn day(m: u32, d: u32) -> Day {
        Day::from_ymd(2026, m, d).unwrap()
    }

    fn closes(s: &Series) -> String {
        let closes = s.closes.iter().map(|(d, p)| format!("{d} {p}"));
        closes.collect::<Vec<_>>().join(", ")
    }

    // 2026-11-12 18:33:10 UTC, when these were fetched.
    const NOW: i64 = 1794501190;

    // XIU.TO, trimmed: TSX bars at 13:30 UTC, the exchange 5 hours behind.
    const XIU: &str = r#"{"chart":{"result":[{"meta":{"currency":"CAD","symbol":"XIU.TO","instrumentType":"ETF","gmtoffset":-18000,"longName":"iShares S&P/TSX 60 Index ETF","shortName":"iSHARES SP TSX 60 INDEX ETF"},"timestamp":[1783085400,1783344600,1783431000],"indicators":{"quote":[{"close":[52.279998779296875,null,52.560001373291016]}],"adjclose":[{}]}}],"error":null}}"#;

    // USDCAD=X, trimmed: London FX bars at midnight summer time, 23:00 UTC
    // the day before, read in November, when the offset is 0.
    const USDCAD: &str = r#"{"chart":{"result":[{"meta":{"currency":"CAD","symbol":"USDCAD=X","instrumentType":"CURRENCY","gmtoffset":0,"longName":"USD/CAD","shortName":"USD/CAD"},"timestamp":[1783292400,1783378800],"indicators":{"quote":[{"close":[1.4204200506210327,1.4207799434661865]}]}}],"error":null}}"#;

    // BTC-USD, trimmed: bars at midnight UTC, the last one today's.
    const BTC_USD: &str = r#"{"chart":{"result":[{"meta":{"currency":"USD","symbol":"BTC-USD","instrumentType":"CRYPTOCURRENCY","gmtoffset":0,"longName":"Bitcoin USD","shortName":"Bitcoin USD"},"timestamp":[1794268800,1794355200,1794441600],"indicators":{"quote":[{"close":[73720.1171875,73964.46875,71616.3984375]}]}}],"error":null}}"#;

    // A ticker Yahoo does not know, with its 404.
    const UNKNOWN: &str = r#"{"chart":{"result":null,"error":{"code":"Not Found","description":"No data found, symbol may be delisted"}}}"#;

    // XIU without the exchange: a stub with no currency and no bars.
    const STUB: &str = r#"{"chart":{"result":[{"meta":{"currency":null,"symbol":"XIU","instrumentType":"ECNQUOTE","gmtoffset":-18000},"indicators":{"quote":[{}],"adjclose":[{}]}}],"error":null}}"#;

    // AAPL asked for a weekend: no bars, but a currency.
    const WEEKEND: &str = r#"{"chart":{"result":[{"meta":{"currency":"USD","symbol":"AAPL","instrumentType":"EQUITY","gmtoffset":-18000,"longName":"Apple Inc.","shortName":"Apple Inc."},"indicators":{"quote":[{}],"adjclose":[{}]}}],"error":null}}"#;

    #[test]
    fn exchange_bars_are_read_on_their_days() {
        let s = parse_yahoo(XIU, NOW).unwrap().unwrap();
        assert_eq!(
            (&*s.currency, &*s.name),
            ("CAD", "iShares S&P/TSX 60 Index ETF")
        );
        // the f32 close 52.279998779296875 is written 52.28; the null close
        // of 2026-07-06 is left out
        assert_eq!(closes(&s), "2026-07-03 52.28, 2026-07-07 52.56");
    }

    #[test]
    fn summer_fx_bars_are_read_on_their_days() {
        let s = parse_yahoo(USDCAD, NOW).unwrap().unwrap();
        assert_eq!(closes(&s), "2026-07-06 1.42042, 2026-07-07 1.42078");
    }

    #[test]
    fn todays_bar_is_left_out() {
        let s = parse_yahoo(BTC_USD, NOW).unwrap().unwrap();
        assert_eq!(closes(&s), "2026-11-10 73720.12, 2026-11-11 73964.47");
        // a day later, the bar of 2026-11-12 is over
        let s = parse_yahoo(BTC_USD, NOW + 86400).unwrap().unwrap();
        assert_eq!(s.closes.len(), 3);
    }

    #[test]
    fn unknown_tickers_and_empty_answers() {
        assert_eq!(parse_yahoo(UNKNOWN, NOW), Ok(None));
        assert_eq!(parse_yahoo(STUB, NOW), Ok(None));
        let s = parse_yahoo(WEEKEND, NOW).unwrap().unwrap();
        assert_eq!((&*s.currency, s.closes.len()), ("USD", 0));
        assert!(parse_yahoo("<html>", NOW).is_err());
    }

    #[test]
    fn request_url() {
        assert_eq!(
            yahoo_url("B", "^GSPC", day(7, 6), day(7, 7)),
            "B/v8/finance/chart/%5EGSPC?period1=1783209600&period2=1783555200&interval=1d"
        );
        assert_eq!(encode("USDCAD=X"), "USDCAD=X");
    }

    #[test]
    fn fetch_from_a_local_server() {
        let agent = crate::cli::http::agent(std::time::Duration::from_secs(10));
        let (start, end) = (day(7, 4), day(7, 7));
        let s = yahoo(
            &agent,
            &serve_once("200 OK", XIU),
            "XIU.TO",
            start,
            end,
            NOW,
        )
        .unwrap()
        .unwrap();
        // the bar of 2026-07-03, before the days asked for, is left out
        assert_eq!(closes(&s), "2026-07-07 52.56");
        let unknown = serve_once("404 Not Found", UNKNOWN);
        assert_eq!(yahoo(&agent, &unknown, "NOSUCH", start, end, NOW), Ok(None));
        let failing = serve_once("500 Internal Server Error", "");
        assert!(yahoo(&agent, &failing, "XIU.TO", start, end, NOW).is_err());
    }

    // USD and CAD from 2026-11-05 to 2026-11-11, trimmed: the observation
    // dimension has no weekend, the series are in the order of the currency
    // dimension (CAD, USD), not as asked, and CAD's rate of 2026-11-10 is
    // left out.
    const WEEK: &str = r#"{"dataSets":[{"series":{"0:0:0:0:0":{"observations":{"0":[1.5675],"1":[1.5705],"2":[1.5785],"4":[1.5839]}},"0:1:0:0:0":{"observations":{"0":[1.1224],"1":[1.1165],"2":[1.1193],"3":[1.1214],"4":[1.1127]}}}}],"structure":{"dimensions":{"series":[{"id":"FREQ","values":[{"id":"D"}]},{"id":"CURRENCY","values":[{"id":"CAD","name":"Canadian dollar"},{"id":"USD","name":"US dollar"}]},{"id":"CURRENCY_DENOM","values":[{"id":"EUR"}]},{"id":"EXR_TYPE","values":[{"id":"SP00"}]},{"id":"EXR_SUFFIX","values":[{"id":"A"}]}],"observation":[{"id":"TIME_PERIOD","values":[{"id":"2026-11-05"},{"id":"2026-11-06"},{"id":"2026-11-09"},{"id":"2026-11-10"},{"id":"2026-11-11"}]}]}}}"#;

    // USD and CAD over Christmas 2011, trimmed: the rates of 2011-12-26 are
    // null, as on many holidays up to 2012.
    const HOLIDAY: &str = r#"{"dataSets":[{"series":{"0:0:0:0:0":{"observations":{"0":[1.3309],"1":[null],"2":[1.3322]}},"0:1:0:0:0":{"observations":{"0":[1.3057],"1":[null],"2":[1.3069]}}}}],"structure":{"dimensions":{"series":[{"id":"FREQ","values":[{"id":"D"}]},{"id":"CURRENCY","values":[{"id":"CAD"},{"id":"USD"}]},{"id":"CURRENCY_DENOM","values":[{"id":"EUR"}]},{"id":"EXR_TYPE","values":[{"id":"SP00"}]},{"id":"EXR_SUFFIX","values":[{"id":"A"}]}],"observation":[{"id":"TIME_PERIOD","values":[{"id":"2011-12-23"},{"id":"2011-12-26"},{"id":"2011-12-27"}]}]}}}"#;

    // A currency ECB does not publish, with its 404, trimmed.
    const NO_SERIES: &str =
        r#"{"detail":"No Series was returned for the query","status":404,"title":"Not Found"}"#;

    fn ecb_closes(body: &str, pair: &str) -> Option<String> {
        let (a, b) = ecb_pair(pair).unwrap();
        parse_ecb(body, &a, &b).unwrap().map(|s| closes(&s))
    }

    #[test]
    fn ecb_cross_rates_and_euro_pairs() {
        // CAD's rate over USD's on the days both have one, read from the
        // observation dimension, to six significant digits
        let usd_cad =
            "2026-11-05 1.39656, 2026-11-06 1.40663, 2026-11-09 1.41026, 2026-11-11 1.42347";
        assert_eq!(ecb_closes(WEEK, "USD-CAD").as_deref(), Some(usd_cad));
        let cad_usd =
            "2026-11-05 0.716045, 2026-11-06 0.71092, 2026-11-09 0.709091, 2026-11-11 0.702506";
        assert_eq!(ecb_closes(WEEK, "CAD-USD").as_deref(), Some(cad_usd));
        // with the euro first, the rate as published
        let eur_cad = "2026-11-05 1.5675, 2026-11-06 1.5705, 2026-11-09 1.5785, 2026-11-11 1.5839";
        assert_eq!(ecb_closes(WEEK, "EUR-CAD").as_deref(), Some(eur_cad));
        // with the euro second, one over the rate
        let usd_eur = "2026-11-05 0.890948, 2026-11-06 0.895656, 2026-11-09 0.893416, 2026-11-10 0.891742, 2026-11-11 0.898715";
        assert_eq!(ecb_closes(WEEK, "USD-EUR").as_deref(), Some(usd_eur));
        // null rates are left out
        let holiday = "2011-12-23 1.0193, 2011-12-27 1.01936";
        assert_eq!(ecb_closes(HOLIDAY, "usd-cad").as_deref(), Some(holiday));
        let s = parse_ecb(WEEK, "USD", "CAD").unwrap().unwrap();
        assert_eq!((&*s.currency, &*s.name), ("CAD", "USD-CAD"));
    }

    #[test]
    fn ecb_empty_and_unknown() {
        // ECB's answer for days without rates, such as a weekend
        let s = parse_ecb("", "USD", "CAD").unwrap().unwrap();
        assert_eq!((&*s.currency, s.closes.len()), ("CAD", 0));
        // rates, but none for a currency of the pair
        assert_eq!(ecb_closes(WEEK, "USD-JPY"), None);
        assert_eq!(ecb_closes(WEEK, "EUR-JPY"), None);
        assert!(parse_ecb("<html>", "USD", "CAD").is_err());
    }

    #[test]
    fn ecb_pairs_and_requests() {
        assert_eq!(ecb_pair("usd-cad"), Some(("USD".into(), "CAD".into())));
        for t in "USDCAD US-CAD USD-CAD-EUR USD- US$-CAD EUR-EUR".split(' ') {
            assert_eq!(ecb_pair(t), None, "{t}");
            assert!(Source::Ecb.check_ticker(t).is_err());
        }
        assert_eq!(
            ecb_url("B", "USD", "CAD", day(11, 5), day(11, 11)),
            "B/service/data/EXR/D.USD+CAD.EUR.SP00.A?startPeriod=2026-11-05&endPeriod=2026-11-11&format=jsondata&detail=dataonly"
        );
        // only the currencies other than the euro are asked for
        let url = |a: &str, b: &str| ecb_url("B", a, b, day(11, 5), day(11, 11));
        assert!(url("EUR", "CAD").starts_with("B/service/data/EXR/D.CAD.EUR.SP00.A?"));
        assert!(url("USD", "EUR").starts_with("B/service/data/EXR/D.USD.EUR.SP00.A?"));
        let six = |x: &str| six_digits(&x.parse().unwrap()).to_string();
        assert_eq!(
            [six("1234567.8"), six("0.00123456789"), six("9.9999996")],
            ["1234568", "0.00123457", "10"]
        );
    }

    #[test]
    fn ecb_fetch_from_a_local_server() {
        let agent = crate::cli::http::agent(std::time::Duration::from_secs(10));
        let (start, end) = (day(11, 5), day(11, 11));
        let s = ecb(&agent, &serve_once("200 OK", WEEK), "EUR-CAD", start, end)
            .unwrap()
            .unwrap();
        assert_eq!(
            closes(&s),
            "2026-11-05 1.5675, 2026-11-06 1.5705, 2026-11-09 1.5785, 2026-11-11 1.5839"
        );
        let empty = serve_once("200 OK", "");
        let s = ecb(&agent, &empty, "USD-CAD", start, end).unwrap().unwrap();
        assert_eq!(s.closes.len(), 0);
        let unknown = serve_once("404 Not Found", NO_SERIES);
        assert_eq!(ecb(&agent, &unknown, "USD-XYZ", start, end), Ok(None));
    }
}
