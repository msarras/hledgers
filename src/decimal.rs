//! Decimal numbers with up to 255 decimal places: the `Decimal` type of the
//! Decimal package (Data.Decimal 0.5.2), which hledger uses for quantities.
//!
//! A value is `mantissa / 10^places`. The places are part of the
//! representation, not the value: 1.0 and 1.00 are equal (and hash alike) but
//! display differently. As in Data.Decimal, addition and subtraction keep the
//! larger number of places, negation keeps the places, multiplication gives
//! the exact product with the fewest places that show it, and division gives
//! the exact quotient rounded half-even at 255 places, then with the fewest
//! places that show that.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::ops::{Add, Div, Mul, Neg, Sub};
use std::str::FromStr;

use dashu_int::IBig;
use dashu_int::ops::{Abs, DivRem, Gcd, UnsignedAbs};

/// The most decimal places a Decimal can have.
const MAX_PLACES: u32 = 255;

#[derive(Clone, Debug)]
pub struct Decimal {
    mantissa: IBig,
    places: u8,
}

impl Decimal {
    pub const ZERO: Decimal = Decimal {
        mantissa: IBig::ZERO,
        places: 0,
    };

    /// The value `mantissa / 10^places`.
    pub fn new(mantissa: IBig, places: u8) -> Decimal {
        Decimal { mantissa, places }
    }

    pub fn from_i64(n: i64) -> Decimal {
        Decimal::new(IBig::from(n), 0)
    }

    pub fn places(&self) -> u8 {
        self.places
    }

    pub fn mantissa(&self) -> &IBig {
        &self.mantissa
    }

    pub fn is_zero(&self) -> bool {
        self.mantissa.is_zero()
    }

    pub fn is_negative(&self) -> bool {
        self.mantissa < IBig::ZERO
    }

    /// The absolute value, with the same places.
    pub fn abs(&self) -> Decimal {
        Decimal::new((&self.mantissa).abs(), self.places)
    }

    /// -1, 0 or 1, with no places.
    pub fn signum(&self) -> Decimal {
        Decimal::new(self.mantissa.signum(), 0)
    }

    /// This number with exactly `places` decimal places, rounding half to even
    /// if that drops digits (Data.Decimal's roundTo).
    pub fn round_to(&self, places: u8) -> Decimal {
        if self.is_zero() {
            return Decimal::new(IBig::ZERO, places);
        }
        let mantissa = match places.cmp(&self.places) {
            Ordering::Less => div_round(&self.mantissa, &pow10(self.places - places)),
            Ordering::Equal => self.mantissa.clone(),
            Ordering::Greater => &self.mantissa * pow10(places - self.places),
        };
        Decimal::new(mantissa, places)
    }

    /// The same value with the fewest places that show it exactly
    /// (Data.Decimal's normalizeDecimal). Zero has no places.
    pub fn normalize(&self) -> Decimal {
        let ten = IBig::from(10);
        let mut mantissa = self.mantissa.clone();
        let mut places = self.places;
        if mantissa.is_zero() {
            places = 0;
        }
        while places > 0 {
            let (q, r) = (&mantissa).div_rem(&ten);
            if !r.is_zero() {
                break;
            }
            mantissa = q;
            places -= 1;
        }
        Decimal::new(mantissa, places)
    }

    /// The double's exact value, rounded half-even to 255 places if it has
    /// more, with the fewest places that show it (realToFrac from a Double:
    /// fromRational). Zero for infinities and NaN.
    pub fn from_f64(x: f64) -> Decimal {
        if x == 0.0 || !x.is_finite() {
            return Decimal::ZERO;
        }
        let bits = x.to_bits();
        let exponent = ((bits >> 52) & 0x7ff) as i64;
        let fraction = bits & ((1 << 52) - 1);
        // the value is mantissa * 2^power
        let (mantissa, power) = if exponent == 0 {
            (fraction, -1074)
        } else {
            (fraction | (1 << 52), exponent - 1075)
        };
        let mut m = IBig::from(mantissa);
        if x < 0.0 {
            m = -m;
        }
        if power >= 0 {
            return Decimal::new(m << power as usize, 0).normalize();
        }
        // mantissa * 5^k / 10^k
        let k = (-power) as usize;
        let scaled = m * IBig::from(5).pow(k);
        if k <= MAX_PLACES as usize {
            Decimal::new(scaled, k as u8).normalize()
        } else {
            let excess = IBig::from(10).pow(k - MAX_PLACES as usize);
            Decimal::new(div_round(&scaled, &excess), u8::MAX).normalize()
        }
    }

    /// The nearest double (what Haskell's realToFrac gives).
    pub fn to_f64(&self) -> f64 {
        // Rust's float parser rounds correctly, whatever the length of the input.
        format!("{}e-{}", self.mantissa, self.places)
            .parse()
            .expect("a decimal in exponent notation parses as f64")
    }

    /// The number of digits in the mantissa, ignoring its sign.
    pub fn digits_count(&self) -> usize {
        if self.is_zero() {
            1
        } else {
            (&self.mantissa)
                .unsigned_abs()
                .ilog(&dashu_int::UBig::from(10u8))
                + 1
        }
    }

    /// Whether this has the most places, or digits, a Decimal can have, which
    /// suggests that it is the rounded result of a division that does not
    /// terminate, such as 100/3 (hledger's amountHasMaxDigits).
    pub fn has_max_digits(&self) -> bool {
        u32::from(self.places) >= MAX_PLACES || self.digits_count() >= MAX_PLACES as usize
    }

    /// The mantissas of two non-zero numbers rescaled to the larger number of
    /// places, and that number (Data.Decimal's roundMax).
    fn aligned(&self, other: &Decimal) -> (u8, IBig, IBig) {
        match self.places.cmp(&other.places) {
            Ordering::Equal => (self.places, self.mantissa.clone(), other.mantissa.clone()),
            Ordering::Less => (
                other.places,
                &self.mantissa * pow10(other.places - self.places),
                other.mantissa.clone(),
            ),
            Ordering::Greater => (
                self.places,
                self.mantissa.clone(),
                &other.mantissa * pow10(self.places - other.places),
            ),
        }
    }
}

fn pow10(n: u8) -> IBig {
    IBig::from(10).pow(usize::from(n))
}

/// `n / d` rounded to the nearest integer, ties to even, for positive `d`
/// (Data.Decimal's divRound).
fn div_round(n: &IBig, d: &IBig) -> IBig {
    let (q, r) = n.div_rem(d);
    let twice_r = r.abs() * IBig::from(2);
    match twice_r.cmp(d) {
        Ordering::Less => q,
        Ordering::Greater => q + n.signum(),
        Ordering::Equal if (&q % 2i32) != 0 => q + n.signum(),
        Ordering::Equal => q,
    }
}

/// `mantissa / 10^places` for up to 510 places, rounded half-even to at most
/// 255 places, with the fewest places that show the result.
fn rounded_normalized(mantissa: IBig, places: u32) -> Decimal {
    if places <= MAX_PLACES {
        return Decimal::new(mantissa, places as u8).normalize();
    }
    let excess = u8::try_from(places - MAX_PLACES).expect("at most 510 places");
    Decimal::new(div_round(&mantissa, &pow10(excess)), u8::MAX).normalize()
}

impl PartialEq for Decimal {
    fn eq(&self, other: &Decimal) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Decimal {}

impl Ord for Decimal {
    fn cmp(&self, other: &Decimal) -> Ordering {
        if self.places == other.places {
            return self.mantissa.cmp(&other.mantissa);
        }
        let signs = self.mantissa.signum().cmp(&other.mantissa.signum());
        if signs != Ordering::Equal || self.is_zero() {
            return signs;
        }
        let (_, a, b) = self.aligned(other);
        a.cmp(&b)
    }
}

impl PartialOrd for Decimal {
    fn partial_cmp(&self, other: &Decimal) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Hash for Decimal {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let n = self.normalize();
        n.mantissa.hash(state);
        n.places.hash(state);
    }
}

impl Neg for &Decimal {
    type Output = Decimal;

    fn neg(self) -> Decimal {
        Decimal::new(-&self.mantissa, self.places)
    }
}

impl Neg for Decimal {
    type Output = Decimal;

    fn neg(self) -> Decimal {
        Decimal::new(-self.mantissa, self.places)
    }
}

/// Adding zero gives the other operand unchanged, keeping its places.
impl Add for &Decimal {
    type Output = Decimal;

    fn add(self, rhs: &Decimal) -> Decimal {
        if self.is_zero() {
            return rhs.clone();
        }
        if rhs.is_zero() {
            return self.clone();
        }
        let (places, a, b) = self.aligned(rhs);
        Decimal::new(a + b, places)
    }
}

/// Subtracting from zero negates, keeping the places; subtracting zero gives
/// the left operand unchanged.
impl Sub for &Decimal {
    type Output = Decimal;

    fn sub(self, rhs: &Decimal) -> Decimal {
        if self.is_zero() {
            return -rhs;
        }
        if rhs.is_zero() {
            return self.clone();
        }
        let (places, a, b) = self.aligned(rhs);
        Decimal::new(a - b, places)
    }
}

/// The exact product, rounded half-even at 255 places when it has more,
/// with the fewest places that show it (hledger's multiplyQuantities, which
/// gives the same result as Data.Decimal's `*`).
impl Mul for &Decimal {
    type Output = Decimal;

    fn mul(self, rhs: &Decimal) -> Decimal {
        if self.is_zero() || rhs.is_zero() {
            return Decimal::ZERO;
        }
        rounded_normalized(
            &self.mantissa * &rhs.mantissa,
            u32::from(self.places) + u32::from(rhs.places),
        )
    }
}

/// The exact quotient rounded half-even at 255 places, with the fewest places
/// that show it. Panics if the divisor is zero, like Haskell's `/`; hledger
/// checks for that before dividing.
impl Div for &Decimal {
    type Output = Decimal;

    fn div(self, rhs: &Decimal) -> Decimal {
        assert!(!rhs.is_zero(), "Decimal division by zero");
        let mut num = &self.mantissa * pow10(rhs.places);
        let mut den = &rhs.mantissa * pow10(self.places);
        if den < IBig::ZERO {
            num = -num;
            den = -den;
        }
        if let Some(q) = terminating_quotient(&num, &den) {
            return q;
        }
        Decimal::new(div_round(&(num * pow10(u8::MAX)), &den), u8::MAX).normalize()
    }
}

/// `num / den` for positive `den`, if it has at most 255 decimal places,
/// with the fewest places that show it; the same as rounding at 255 places
/// and normalizing, without the 255-digit intermediate. In lowest terms the
/// quotient terminates exactly when the denominator is `2^a * 5^b`, and then
/// it takes `max(a, b)` places, the last digit not zero.
fn terminating_quotient(num: &IBig, den: &IBig) -> Option<Decimal> {
    let g = IBig::from(num.gcd(den));
    let mut d = den / &g;
    let twos = d.trailing_zeros().unwrap_or(0);
    d >>= twos;
    let five = IBig::from(5);
    let mut fives = 0;
    while fives <= MAX_PLACES as usize && (&d % &five).is_zero() {
        d /= &five;
        fives += 1;
    }
    let places = twos.max(fives);
    if d != IBig::ONE || places > MAX_PLACES as usize {
        return None;
    }
    let mantissa = ((num / &g) << (places - twos)) * five.pow(places - fives);
    Some(Decimal::new(mantissa, places as u8))
}

macro_rules! forward_owned_ops {
    ($($op:ident $method:ident),*) => {$(
        impl $op for Decimal {
            type Output = Decimal;

            fn $method(self, rhs: Decimal) -> Decimal {
                (&self).$method(&rhs)
            }
        }
    )*};
}

forward_owned_ops!(Add add, Sub sub, Mul mul, Div div);

/// Like Haskell's `show`: the integer digits, then if there are places, a
/// period and exactly that many digits.
impl fmt::Display for Decimal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let sign = if self.is_negative() { "-" } else { "" };
        let digits = (&self.mantissa).unsigned_abs().to_string();
        let places = usize::from(self.places);
        if places == 0 {
            return write!(f, "{sign}{digits}");
        }
        let zeros = "0".repeat((places + 1).saturating_sub(digits.len()));
        let padded = zeros + &digits;
        let (int, frac) = padded.split_at(padded.len() - places);
        write!(f, "{sign}{int}.{frac}")
    }
}

/// Reads a decimal literal like Data.Decimal's `read` (without the white
/// space that allows around it): an optional sign, digits, optionally a
/// period and more digits, and optionally an exponent (`1.5e3`). The places
/// are the fraction digits minus the exponent (none if that is negative);
/// more than 255 is an error.
impl FromStr for Decimal {
    type Err = String;

    fn from_str(s: &str) -> Result<Decimal, String> {
        let invalid = || format!("not a decimal number: {s:?}");
        let (negative, t) = match s.as_bytes().first() {
            Some(b'-') => (true, &s[1..]),
            Some(b'+') => (false, &s[1..]),
            _ => (false, s),
        };
        let digits = |t: &str| t.bytes().take_while(u8::is_ascii_digit).count();
        let int_len = digits(t);
        if int_len == 0 {
            return Err(invalid());
        }
        let (int, mut rest) = t.split_at(int_len);
        let mut frac = "";
        if let Some(r) = rest.strip_prefix('.') {
            let n = digits(r);
            if n == 0 {
                return Err(invalid());
            }
            (frac, rest) = r.split_at(n);
        }
        let mut exponent: i64 = 0;
        if let Some(r) = rest.strip_prefix(['e', 'E']) {
            let (exp_negative, r) = match r.as_bytes().first() {
                Some(b'-') => (true, &r[1..]),
                Some(b'+') => (false, &r[1..]),
                _ => (false, r),
            };
            let n = digits(r);
            if n == 0 {
                return Err(invalid());
            }
            exponent = r[..n].parse().map_err(|_| invalid())?;
            if exp_negative {
                exponent = -exponent;
            }
            rest = &r[n..];
        }
        if !rest.is_empty() {
            return Err(invalid());
        }
        let mut mantissa: IBig = format!("{int}{frac}").parse().map_err(|_| invalid())?;
        if negative {
            mantissa = -mantissa;
        }
        let places = frac.len() as i64 - exponent;
        if places < 0 {
            let scale = IBig::from(10).pow(usize::try_from(-places).map_err(|_| invalid())?);
            Ok(Decimal::new(mantissa * scale, 0))
        } else if places <= i64::from(MAX_PLACES) {
            Ok(Decimal::new(mantissa, places as u8))
        } else {
            Err(invalid())
        }
    }
}

pub(crate) mod tests {
    use super::*;

    /// A decimal as the literal is written, keeping its places.
    fn dec(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    /// A decimal as a Haskell literal makes it (fromRational): with the
    /// fewest places.
    fn lit(s: &str) -> Decimal {
        dec(s).normalize()
    }

    fn raw(mantissa: i64, places: u8) -> Decimal {
        Decimal::new(IBig::from(mantissa), places)
    }

    /// The representation, for checking places as well as value.
    fn rep(d: &Decimal) -> (String, u8) {
        (d.mantissa.to_string(), d.places)
    }

    #[test]
    fn display_shows_the_stored_places() {
        assert_eq!(raw(1500, 3).to_string(), "1.500");
        assert_eq!(raw(70, 2).to_string(), "0.70");
        assert_eq!(raw(-5, 2).to_string(), "-0.05");
        assert_eq!(raw(-12, 0).to_string(), "-12");
        assert_eq!(raw(0, 2).to_string(), "0.00");
        assert_eq!(raw(123, 2).to_string(), "1.23");
    }

    #[test]
    fn parse_literals() {
        assert_eq!(rep(&dec("1.50")), ("150".into(), 2));
        assert_eq!(rep(&dec("-0.0100")), ("-100".into(), 4));
        assert_eq!(rep(&dec("+3")), ("3".into(), 0));
        assert_eq!(rep(&dec("1.5e3")), ("1500".into(), 0));
        assert_eq!(rep(&dec("1.5E-3")), ("15".into(), 4));
        assert_eq!(rep(&dec("1e-255")), ("1".into(), 255));
        assert_eq!(rep(&dec("12e+1")), ("120".into(), 0));
        for bad in [
            "", "-", "1.", ".5", "1.5e", "1e-256", "abc", "1 2", "1,5", "--1", " 7",
        ] {
            assert!(bad.parse::<Decimal>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn from_doubles() {
        let d = |x: f64| Decimal::from_f64(x).to_string();
        assert_eq!(d(0.5), "0.5");
        assert_eq!(d(-2.0), "-2");
        assert_eq!(d(1e20), "100000000000000000000");
        assert_eq!(
            d(0.1),
            "0.1000000000000000055511151231257827021181583404541015625"
        );
        // more than 255 places are rounded
        assert_eq!(Decimal::from_f64(1e-78).places(), 255);
        assert!(Decimal::from_f64(1e-300).is_zero());
        assert_eq!(Decimal::from_f64(0.1).to_f64(), 0.1);
    }

    #[test]
    fn addition_aligns_places_and_skips_zeros() {
        assert_eq!(rep(&(dec("1.5") + dec("0.25"))), ("175".into(), 2));
        assert_eq!(rep(&(dec("1.50") + dec("-1.5"))), ("0".into(), 2));
        // Adding zero gives the other operand, whatever the zero's places.
        assert_eq!(rep(&(raw(0, 3) + raw(5, 1))), ("5".into(), 1));
        assert_eq!(rep(&(raw(5, 1) + raw(0, 3))), ("5".into(), 1));
        assert_eq!(rep(&(raw(0, 3) + raw(0, 1))), ("0".into(), 1));
    }

    #[test]
    fn subtraction_and_negation_keep_places() {
        assert_eq!(rep(&(dec("1.5") - dec("0.25"))), ("125".into(), 2));
        assert_eq!(rep(&(raw(0, 2) - raw(5, 1))), ("-5".into(), 1));
        assert_eq!(rep(&(raw(5, 1) - raw(0, 3))), ("5".into(), 1));
        assert_eq!(rep(&-raw(0, 2)), ("0".into(), 2));
        assert_eq!(rep(&-raw(5, 3)), ("-5".into(), 3));
    }

    // Amount.hs tests_Amount "multiplyQuantities": the same result and
    // representation as Data.Decimal's `*`, which is the only `*` here.
    #[cfg_attr(test, test)]
    pub(crate) fn multiplication_is_exact_and_normalized() {
        assert_eq!(rep(&(lit("0.71") * lit("3"))), ("213".into(), 2));
        assert_eq!(rep(&(lit("1.50") * lit("-2.0"))), ("-3".into(), 0));
        assert_eq!(rep(&(lit("0") * lit("100"))), ("0".into(), 0));
        assert_eq!(rep(&(raw(3, 200) * raw(5, 100))), ("0".into(), 0));
        assert_eq!(lit("2.50") * lit("4"), lit("10"));
        assert_eq!(rep(&(lit("2.50") * lit("4"))), ("10".into(), 0));
        // Data.Decimal's documentation example.
        assert_eq!(rep(&(dec("0.15") * dec("0.15"))), ("225".into(), 4));
        // More than 255 places: rounded half-even at 255.
        assert_eq!(rep(&(raw(15, 200) * raw(1, 56))), ("2".into(), 255));
        assert_eq!(rep(&(raw(25, 200) * raw(1, 56))), ("2".into(), 255));
        assert_eq!(rep(&(raw(-35, 200) * raw(1, 56))), ("-4".into(), 255));
    }

    #[test]
    fn division_rounds_half_even_at_255_places() {
        let third = dec("1") / dec("3");
        assert_eq!(third.places(), 255);
        assert_eq!(third.to_string(), format!("0.{}", "3".repeat(255)));
        let two_thirds = dec("2") / dec("3");
        assert_eq!(two_thirds.to_string(), format!("0.{}7", "6".repeat(254)));
        assert_eq!(rep(&(dec("1") / dec("4"))), ("25".into(), 2));
        assert_eq!(rep(&(dec("-1.50") / dec("0.5"))), ("-3".into(), 0));
        assert_eq!(rep(&(dec("3") / dec("-0.5"))), ("-6".into(), 0));
        assert_eq!(rep(&(raw(0, 4) / dec("7"))), ("0".into(), 0));
        // Ties at the 255th place go to the even neighbour.
        assert_eq!(rep(&(raw(1, 255) / dec("2"))), ("0".into(), 0));
        assert_eq!(rep(&(raw(3, 255) / dec("2"))), ("2".into(), 255));
        assert_eq!(rep(&(raw(-3, 255) / dec("2"))), ("-2".into(), 255));
    }

    // Amount.hs, the note under amountHasMaxDigits: 100/3 has 255 places and
    // 257 mantissa digits.
    #[test]
    fn has_max_digits() {
        let n = dec("100") / dec("3.0");
        assert_eq!(n.places(), 255);
        assert_eq!(n.digits_count(), 257);
        assert!(n.has_max_digits());
        assert!((dec("1") / dec("3")).has_max_digits());
        assert!(!(dec("1") / dec("4")).has_max_digits());
        assert!(Decimal::new(IBig::from(10).pow(254), 0).has_max_digits());
        assert!(!Decimal::new(IBig::from(10).pow(253), 0).has_max_digits());
        assert_eq!(raw(0, 3).digits_count(), 1);
        assert_eq!(raw(-999, 1).digits_count(), 3);
    }

    #[test]
    fn round_to_is_half_even() {
        assert_eq!(dec("0.125").round_to(2).to_string(), "0.12");
        assert_eq!(dec("0.135").round_to(2).to_string(), "0.14");
        assert_eq!(dec("-0.125").round_to(2).to_string(), "-0.12");
        assert_eq!(dec("-0.135").round_to(2).to_string(), "-0.14");
        assert_eq!(dec("2.5").round_to(0).to_string(), "2");
        assert_eq!(dec("3.5").round_to(0).to_string(), "4");
        assert_eq!(dec("0.126").round_to(2).to_string(), "0.13");
        assert_eq!(dec("1.5").round_to(3).to_string(), "1.500");
        assert_eq!(rep(&raw(0, 2).round_to(5)), ("0".into(), 5));
        assert_eq!(dec("-0.004").round_to(2).to_string(), "0.00");
    }

    #[test]
    fn normalize_strips_trailing_zeros() {
        assert_eq!(rep(&dec("1.500").normalize()), ("15".into(), 1));
        assert_eq!(rep(&dec("0.000").normalize()), ("0".into(), 0));
        assert_eq!(rep(&dec("100").normalize()), ("100".into(), 0));
        assert_eq!(rep(&dec("-2.0").normalize()), ("-2".into(), 0));
    }

    #[test]
    fn equality_order_and_hash_are_by_value() {
        use std::collections::hash_map::DefaultHasher;
        let hash = |d: &Decimal| {
            let mut h = DefaultHasher::new();
            d.hash(&mut h);
            h.finish()
        };
        assert_eq!(dec("1.0"), dec("1.00"));
        assert_eq!(hash(&dec("1.0")), hash(&dec("1.00")));
        assert_eq!(dec("0.000"), Decimal::ZERO);
        assert_eq!(hash(&dec("0.000")), hash(&Decimal::ZERO));
        assert!(dec("-1") < dec("0.5"));
        assert!(dec("0.10") > dec("0.09"));
        assert!(dec("-0.10") < dec("-0.09"));
        assert!(dec("2") > dec("1.999"));
        assert!(dec("0.000") < dec("0.001"));
        assert!(dec("-0.001") < raw(0, 0));
    }

    #[test]
    fn unary_functions() {
        assert_eq!(rep(&dec("-1.50").abs()), ("150".into(), 2));
        assert_eq!(rep(&dec("-1.50").signum()), ("-1".into(), 0));
        assert_eq!(rep(&dec("0.00").signum()), ("0".into(), 0));
        assert_eq!(rep(&Decimal::from_i64(-7)), ("-7".into(), 0));
        assert!(dec("-0.1").is_negative());
        assert!(!dec("0.0").is_negative());
    }

    #[test]
    fn to_f64_is_the_nearest_double() {
        assert_eq!(dec("0.1").to_f64(), 0.1);
        assert_eq!(dec("123.45").to_f64(), 123.45);
        assert_eq!(dec("-2").to_f64(), -2.0);
        assert_eq!((dec("1") / dec("3")).to_f64(), 1.0 / 3.0);
        assert_eq!(raw(0, 9).to_f64(), 0.0);
    }
}
