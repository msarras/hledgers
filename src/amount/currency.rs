//! Currency names, symbols and ISO 4217 codes (Hledger.Data.Currency).
//! Reference: <https://www.xe.com/symbols>, <https://www.xe.com/currency>.

/// The ISO 4217 code for a traditional currency symbol, such as USD for `$`.
/// Where several currencies share a symbol, the last in the table wins, as
/// in upstream's map: `$` is USD, `£` GBP, `¥` JPY.
pub fn currency_symbol_to_code(symbol: &str) -> Option<&'static str> {
    CURRENCIES
        .iter()
        .rev()
        .find(|(_, _, s)| *s == symbol)
        .map(|&(_, code, _)| code)
}

/// The ISO 4217 code for a currency symbol, or the symbol itself if it has
/// none, for comparing commodities written either way.
pub fn to_currency_code(symbol: &str) -> &str {
    currency_symbol_to_code(symbol).unwrap_or(symbol)
}

/// Country and currency name, ISO 4217 code, symbol.
const CURRENCIES: &[(&str, &str, &str)] = &[
    ("Albania Lek", "ALL", "Lek"),
    ("Afghanistan Afghani", "AFN", "؋"),
    ("Argentina Peso", "ARS", "$"),
    ("Aruba Guilder", "AWG", "ƒ"),
    ("Australia Dollar", "AUD", "$"),
    ("Azerbaijan Manat", "AZN", "₼"),
    ("Bahamas Dollar", "BSD", "$"),
    ("Barbados Dollar", "BBD", "$"),
    ("Belarus Ruble", "BYN", "Br"),
    ("Belize Dollar", "BZD", "BZ$"),
    ("Bermuda Dollar", "BMD", "$"),
    ("Bolivia Bolíviano", "BOB", "$b"),
    ("Bosnia and Herzegovina Convertible Mark", "BAM", "KM"),
    ("Botswana Pula", "BWP", "P"),
    ("Bulgaria Lev", "BGN", "лв"),
    ("Brazil Real", "BRL", "R$"),
    ("Brunei Darussalam Dollar", "BND", "$"),
    ("Cambodia Riel", "KHR", "៛"),
    ("Canada Dollar", "CAD", "$"),
    ("Cayman Islands Dollar", "KYD", "$"),
    ("Chile Peso", "CLP", "$"),
    ("China Yuan Renminbi", "CNY", "¥"),
    ("Colombia Peso", "COP", "$"),
    ("Costa Rica Colon", "CRC", "₡"),
    ("Croatia Kuna", "HRK", "kn"),
    ("Cuba Peso", "CUP", "₱"),
    ("Czech Republic Koruna", "CZK", "Kč"),
    ("Denmark Krone", "DKK", "kr"),
    ("Dominican Republic Peso", "DOP", "RD$"),
    ("East Caribbean Dollar", "XCD", "$"),
    ("Egypt Pound", "EGP", "£"),
    ("El Salvador Colon", "SVC", "$"),
    ("Euro Member Countries", "EUR", "€"),
    ("Falkland Islands (Malvinas) Pound", "FKP", "£"),
    ("Fiji Dollar", "FJD", "$"),
    ("Ghana Cedi", "GHS", "¢"),
    ("Gibraltar Pound", "GIP", "£"),
    ("Guatemala Quetzal", "GTQ", "Q"),
    ("Guernsey Pound", "GGP", "£"),
    ("Guyana Dollar", "GYD", "$"),
    ("Honduras Lempira", "HNL", "L"),
    ("Hong Kong Dollar", "HKD", "$"),
    ("Hungary Forint", "HUF", "Ft"),
    ("Iceland Krona", "ISK", "kr"),
    ("India Rupee", "INR", "₹"),
    ("Indonesia Rupiah", "IDR", "Rp"),
    ("Iran Rial", "IRR", "﷼"),
    ("Isle of Man Pound", "IMP", "£"),
    ("Israel Shekel", "ILS", "₪"),
    ("Jamaica Dollar", "JMD", "J$"),
    ("Japan Yen", "JPY", "¥"),
    ("Jersey Pound", "JEP", "£"),
    ("Kazakhstan Tenge", "KZT", "лв"),
    ("Korea (North) Won", "KPW", "₩"),
    ("Korea (South) Won", "KRW", "₩"),
    ("Kyrgyzstan Som", "KGS", "лв"),
    ("Laos Kip", "LAK", "₭"),
    ("Lebanon Pound", "LBP", "£"),
    ("Liberia Dollar", "LRD", "$"),
    ("Macedonia Denar", "MKD", "ден"),
    ("Malaysia Ringgit", "MYR", "RM"),
    ("Mauritius Rupee", "MUR", "₨"),
    ("Mexico Peso", "MXN", "$"),
    ("Mongolia Tughrik", "MNT", "₮"),
    ("Mozambique Metical", "MZN", "MT"),
    ("Namibia Dollar", "NAD", "$"),
    ("Nepal Rupee", "NPR", "₨"),
    ("Netherlands Antilles Guilder", "ANG", "ƒ"),
    ("New Zealand Dollar", "NZD", "$"),
    ("Nicaragua Cordoba", "NIO", "C$"),
    ("Nigeria Naira", "NGN", "₦"),
    ("Norway Krone", "NOK", "kr"),
    ("Oman Rial", "OMR", "﷼"),
    ("Pakistan Rupee", "PKR", "₨"),
    ("Panama Balboa", "PAB", "B/."),
    ("Paraguay Guarani", "PYG", "Gs"),
    ("Peru Sol", "PEN", "S/."),
    ("Philippines Peso", "PHP", "₱"),
    ("Poland Zloty", "PLN", "zł"),
    ("Qatar Riyal", "QAR", "﷼"),
    ("Romania Leu", "RON", "lei"),
    ("Russia Ruble", "RUB", "₽"),
    ("Saint Helena Pound", "SHP", "£"),
    ("Saudi Arabia Riyal", "SAR", "﷼"),
    ("Serbia Dinar", "RSD", "Дин."),
    ("Seychelles Rupee", "SCR", "₨"),
    ("Singapore Dollar", "SGD", "$"),
    ("Solomon Islands Dollar", "SBD", "$"),
    ("Somalia Shilling", "SOS", "S"),
    ("South Africa Rand", "ZAR", "R"),
    ("Sri Lanka Rupee", "LKR", "₨"),
    ("Sweden Krona", "SEK", "kr"),
    ("Switzerland Franc", "CHF", "CHF"),
    ("Suriname Dollar", "SRD", "$"),
    ("Syria Pound", "SYP", "£"),
    ("Taiwan New Dollar", "TWD", "NT$"),
    ("Thailand Baht", "THB", "฿"),
    ("Trinidad and Tobago Dollar", "TTD", "TT$"),
    ("Turkey Lira", "TRY", "₺"),
    ("Tuvalu Dollar", "TVD", "$"),
    ("Ukraine Hryvnia", "UAH", "₴"),
    ("United Kingdom Pound", "GBP", "£"),
    ("United States Dollar", "USD", "$"),
    ("Uruguay Peso", "UYU", "$U"),
    ("Uzbekistan Som", "UZS", "лв"),
    ("Venezuela Bolívar", "VEF", "Bs"),
    ("Viet Nam Dong", "VND", "₫"),
    ("Yemen Rial", "YER", "﷼"),
    ("Zimbabwe Dollar", "ZWD", "Z$"),
];

pub(crate) mod tests {
    use super::*;

    // The doctests of currencySymbolToCode and toCurrencyCode.
    #[cfg_attr(test, test)]
    pub(crate) fn symbols_and_codes() {
        assert_eq!(currency_symbol_to_code(""), None);
        assert_eq!(currency_symbol_to_code("$"), Some("USD"));
        assert_eq!(to_currency_code("$"), "USD");
        assert_eq!(to_currency_code("BTC"), "BTC");
        // Where a symbol is shared, the last currency in the table has it.
        assert_eq!(currency_symbol_to_code("£"), Some("GBP"));
        assert_eq!(currency_symbol_to_code("¥"), Some("JPY"));
        assert_eq!(currency_symbol_to_code("kr"), Some("SEK"));
        assert_eq!(currency_symbol_to_code("Kč"), Some("CZK"));
        assert_eq!(CURRENCIES.len(), 109);
    }
}
