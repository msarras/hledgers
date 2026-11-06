//! Property tests of Decimal against exact rational arithmetic, done
//! independently with num-bigint: for random operands with 0 to 30 places
//! and mantissas of up to 40 digits, `+`, `-`, `*` give the exact result, `/`
//! the exact quotient rounded half-even at 255 places, `round_to` the exact
//! value rounded half-even, and the order is the rationals' order. The
//! operands come from a fixed-seed generator, so failures reproduce.

use std::cmp::Ordering;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use dashu_int::IBig;
use hledgers::decimal::Decimal;
use num_bigint::{BigInt, Sign};

const CASES: usize = 4000;

/// splitmix64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }

    /// A decimal with 0 to 30 places and 1 to 40 digits, either sign, zero
    /// one time in sixteen.
    fn decimal(&mut self) -> Decimal {
        let places = self.below(31) as u8;
        let mut digits = String::new();
        if self.below(2) == 0 {
            digits.push('-');
        }
        if self.below(16) == 0 {
            digits.push('0');
        } else {
            digits.push(char::from(b'1' + self.below(9) as u8));
            for _ in 1..=self.below(40) {
                digits.push(char::from(b'0' + self.below(10) as u8));
            }
        }
        Decimal::new(digits.parse().unwrap(), places)
    }
}

/// A rational number num/den, den > 0.
#[derive(Clone, Debug)]
struct Ratio {
    num: BigInt,
    den: BigInt,
}

fn pow10(e: u32) -> BigInt {
    BigInt::from(10u32).pow(e)
}

fn ratio(d: &Decimal) -> Ratio {
    Ratio {
        num: d.mantissa().to_string().parse().unwrap(),
        den: pow10(u32::from(d.places())),
    }
}

fn add(a: &Ratio, b: &Ratio) -> Ratio {
    Ratio {
        num: &a.num * &b.den + &b.num * &a.den,
        den: &a.den * &b.den,
    }
}

fn neg(a: &Ratio) -> Ratio {
    Ratio {
        num: -&a.num,
        den: a.den.clone(),
    }
}

fn mul(a: &Ratio, b: &Ratio) -> Ratio {
    Ratio {
        num: &a.num * &b.num,
        den: &a.den * &b.den,
    }
}

fn div(a: &Ratio, b: &Ratio) -> Ratio {
    let (num, den) = (&a.num * &b.den, &a.den * &b.num);
    if den.sign() == Sign::Minus {
        Ratio {
            num: -num,
            den: -den,
        }
    } else {
        Ratio { num, den }
    }
}

fn cmp(a: &Ratio, b: &Ratio) -> Ordering {
    (&a.num * &b.den).cmp(&(&b.num * &a.den))
}

/// num/den rounded to the nearest integer, ties to the even one.
fn round_half_even(r: &Ratio) -> BigInt {
    let q = &r.num / &r.den;
    let rem = &r.num % &r.den;
    let twice: BigInt = BigInt::from(rem.magnitude().clone()) * 2;
    let away = if r.num.sign() == Sign::Minus { -1 } else { 1 };
    match twice.cmp(&r.den) {
        Ordering::Less => q,
        Ordering::Greater => q + away,
        Ordering::Equal if (&q % 2) != BigInt::ZERO => q + away,
        Ordering::Equal => q,
    }
}

/// The value rounded half-even to `places` places, as (mantissa, places).
fn round_to(r: &Ratio, places: u32) -> (BigInt, u32) {
    let scaled = Ratio {
        num: &r.num * pow10(places),
        den: r.den.clone(),
    };
    (round_half_even(&scaled), places)
}

/// The fewest places that show mantissa/10^places exactly.
fn normalized((mut m, mut places): (BigInt, u32)) -> (BigInt, u32) {
    if m == BigInt::ZERO {
        return (m, 0);
    }
    while places > 0 && (&m % 10) == BigInt::ZERO {
        m /= 10;
        places -= 1;
    }
    (m, places)
}

/// A decimal's representation, in the oracle's terms.
fn rep(d: &Decimal) -> (BigInt, u32) {
    (
        d.mantissa().to_string().parse().unwrap(),
        u32::from(d.places()),
    )
}

fn hash(d: &Decimal) -> u64 {
    let mut h = DefaultHasher::new();
    d.hash(&mut h);
    h.finish()
}

#[test]
fn decimal_matches_exact_rationals() {
    let mut rng = Rng(0x00c0_ffee_d00d_f00d);
    for case in 0..CASES {
        let (a, b) = (rng.decimal(), rng.decimal());
        let (ra, rb) = (ratio(&a), ratio(&b));
        let ctx = || format!("case {case}: a = {a}, b = {b}");

        // Sums and differences: exact, at the larger places, except that a
        // zero operand gives the other one unchanged.
        let sum = &a + &b;
        assert_eq!(
            cmp(&ratio(&sum), &add(&ra, &rb)),
            Ordering::Equal,
            "{}",
            ctx()
        );
        let places = match (a.is_zero(), b.is_zero()) {
            (true, _) => b.places(),
            (false, true) => a.places(),
            _ => a.places().max(b.places()),
        };
        assert_eq!(sum.places(), places, "{}", ctx());
        let difference = &a - &b;
        assert_eq!(
            cmp(&ratio(&difference), &add(&ra, &neg(&rb))),
            Ordering::Equal,
            "{}",
            ctx()
        );
        let places = match (a.is_zero(), b.is_zero()) {
            (true, _) => b.places(),
            (false, true) => a.places(),
            _ => a.places().max(b.places()),
        };
        assert_eq!(difference.places(), places, "{}", ctx());

        // Products: exact (at most 60 places here), normalized.
        let product = &a * &b;
        let exact = round_to(&mul(&ra, &rb), 60);
        assert_eq!(rep(&product), normalized(exact), "{}", ctx());

        // Quotients: rounded half-even at 255 places, normalized.
        if !b.is_zero() {
            let quotient = &a / &b;
            assert_eq!(
                rep(&quotient),
                normalized(round_to(&div(&ra, &rb), 255)),
                "{}",
                ctx()
            );
        }

        // Rounding to a number of places, half-even.
        let places = rng.below(41) as u8;
        assert_eq!(
            rep(&a.round_to(places)),
            round_to(&ra, u32::from(places)),
            "{} to {places}",
            ctx()
        );

        // Order and equality by value, and hashing consistent with them.
        assert_eq!(a.cmp(&b), cmp(&ra, &rb), "{}", ctx());
        assert_eq!(a == b, cmp(&ra, &rb) == Ordering::Equal, "{}", ctx());
        let extra = rng.below(u64::from(255 - a.places()) + 1) as u8;
        let rescaled = Decimal::new(
            a.mantissa() * IBig::from(10).pow(usize::from(extra)),
            a.places() + extra,
        );
        assert_eq!(rescaled, a, "{}", ctx());
        assert_eq!(hash(&rescaled), hash(&a), "{}", ctx());
        assert_eq!(
            rescaled.normalize().places(),
            a.normalize().places(),
            "{}",
            ctx()
        );

        // Display reads back as the same representation.
        let shown: Decimal = a.to_string().parse().unwrap();
        assert_eq!(rep(&shown), rep(&a), "{}", ctx());
    }
}

/// Operands whose products have more than 255 places are rounded half-even
/// at 255, then normalized.
#[test]
fn long_products_round_at_255_places() {
    let mut rng = Rng(0x5eed);
    for case in 0..CASES {
        let a = Decimal::new(rng.decimal().mantissa().clone(), 120 + rng.below(136) as u8);
        let b = Decimal::new(rng.decimal().mantissa().clone(), 120 + rng.below(136) as u8);
        let exact = mul(&ratio(&a), &ratio(&b));
        assert_eq!(
            rep(&(&a * &b)),
            normalized(round_to(&exact, 255)),
            "case {case}: {a} * {b}"
        );
    }
}

/// Quotients that terminate, at up to 255 places or beyond: a product
/// divided by one factor, and numbers divided by `2^i * 5^j` at some
/// places, which give quotients of up to 330 places; rounded half-even
/// at 255 places, normalized.
#[test]
fn terminating_quotients_round_at_255_places() {
    let mut rng = Rng(0x7e2a_1fac);
    for case in 0..CASES {
        let (a, b) = (rng.decimal(), rng.decimal());
        if !b.is_zero() {
            let product = &a * &b;
            let exact = div(&ratio(&product), &ratio(&b));
            assert_eq!(
                rep(&(&product / &b)),
                normalized(round_to(&exact, 255)),
                "case {case}: {product} / {b}"
            );
        }
        let (i, j) = (rng.below(300) as u32, rng.below(300) as u32);
        let mantissa = BigInt::from(2u32).pow(i) * BigInt::from(5u32).pow(j);
        let divisor = Decimal::new(mantissa.to_string().parse().unwrap(), rng.below(31) as u8);
        let exact = div(&ratio(&a), &ratio(&divisor));
        assert_eq!(
            rep(&(&a / &divisor)),
            normalized(round_to(&exact, 255)),
            "case {case}: {a} / {divisor}"
        );
    }
}
