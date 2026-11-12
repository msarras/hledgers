//! The sources `get` fetches prices from when the journal has no
//! `prices/getprices` helper: each one's request, and the reading of its
//! response, a pure function of the response and the time.

use std::collections::BTreeMap;

use crate::dates::Day;
use crate::decimal::Decimal;

/// A source of prices, as a three-word `price:` tag names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Yahoo,
}

impl Source {
    pub const ALL: [Source; 1] = [Source::Yahoo];

    pub fn name(self) -> &'static str {
        match self {
            Source::Yahoo => "yahoo",
        }
    }

    /// The source with this name.
    pub fn parse(name: &str) -> Option<Source> {
        Source::ALL.into_iter().find(|s| s.name() == name)
    }

    /// The last day whose price is final: yesterday, a day's close being
    /// final only when the day is over.
    pub fn last_day(self, today: Day) -> Day {
        match self {
            Source::Yahoo => today.add_days(-1),
        }
    }

    /// A ticker's closing prices on the days from `start` to `end` that are
    /// over where it trades, in one request; None if the source does not
    /// know the ticker. `now` is the time in seconds since 1970.
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
}
