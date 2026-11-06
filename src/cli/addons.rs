//! Add-on commands: `hledger-*` executables found in PATH
//! (addonCommandNames and installedAddonCommandNames in
//! Hledger.Cli.Commands).

/// The file extensions an add-on executable may have (addonExtensions).
const ADDON_EXTENSIONS: [&str; 15] = [
    "bat", "com", "exe", "hs", "js", "lhs", "lua", "php", "pl", "py", "rb", "rkt", "sh", "osh",
    "ysh",
];

/// The names of the add-on commands in PATH that no builtin command's name
/// or alias shadows (addonCommandNames).
pub fn addon_command_names() -> Vec<String> {
    installed_addon_command_names()
        .into_iter()
        .filter(|a| super::commands::find(drop_extension(a)).is_none())
        .collect()
}

/// The names of the `hledger-*` executables in PATH without that prefix,
/// sorted: with their extension only where it is needed to tell several
/// apart, a source file being dropped beside the compiled program of the
/// same name (installedAddonCommandNames).
fn installed_addon_command_names() -> Vec<String> {
    let mut names: Vec<String> = super::docs::likely_executables_in_path()
        .into_iter()
        .filter(|n| is_hledger_exe_name(n))
        .map(|n| n["hledger-".len()..].to_string())
        .collect();
    // groupSortOn takeBaseName: a stable sort by base name, then groups
    names.sort_by(|a, b| drop_extension(a).cmp(drop_extension(b)));
    let mut out = Vec::new();
    for group in names.chunk_by(|a, b| drop_extension(a) == drop_extension(b)) {
        let compiled = |f: &str| {
            let ext = f.len() - drop_extension(f).len();
            ["", ".com", ".exe"].contains(&f[f.len() - ext..].to_lowercase().as_str())
        };
        match group {
            [f, _] if compiled(f) => out.push(f.clone()),
            [_, g] if compiled(g) => out.push(g.clone()),
            fs => out.extend(fs.iter().cloned()),
        }
    }
    out
}

/// Whether a file name is an add-on's: `hledger-`, a name without periods,
/// and optionally one of the add-on extensions (isHledgerExeName).
fn is_hledger_exe_name(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("hledger-") else {
        return false;
    };
    let (base, ext) = match rest.split_once('.') {
        Some((b, e)) => (b, Some(e)),
        None => (rest, None),
    };
    !base.is_empty() && ext.is_none_or(|e| ADDON_EXTENSIONS.contains(&e))
}

/// A file name without its extension, if any (dropExtension, takeBaseName).
fn drop_extension(f: &str) -> &str {
    match f.rfind('.') {
        Some(i) if i > 0 => &f[..i],
        _ => f,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addon_executable_names() {
        assert!(is_hledger_exe_name("hledger-addon"));
        assert!(is_hledger_exe_name("hledger-addon.lhs"));
        assert!(!is_hledger_exe_name("hledger-addon.hsx"));
        assert!(!is_hledger_exe_name("hledger-"));
        assert!(!is_hledger_exe_name("hledger-a.b.sh"));
        assert!(!is_hledger_exe_name("hledgerx"));
    }
}
