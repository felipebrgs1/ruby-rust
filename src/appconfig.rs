//! calisto — appconfig
//!
//! calisto.toml (AppConfig), dotenv e helpers de app.
//! Extraido de src/main.rs na reorganizacao do CLI (estrutura inspirada no cli/ do Deno).
//! calisto — appconfig (extraido de src/main.rs na reorg do CLI).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use crate::runtime::*;






/// Preload default: so os modulos BARATOS (<=4ms cada, ~22ms no total).
/// Os pesados (yaml/psych ~7ms, uri ~9ms, net/http ~20ms, csv ~7ms — 43ms)
/// ficam de fora: o child paga o load UMA vez quando o script usa (require
/// on demand, CoW — nao polui o daemon), e o cold do daemon cai ~43ms. Apps
/// com preload proprio (calisto.toml) e `--preload`/CALISTO_PRELOAD nao sao
/// afetados — o default e so o daemon generico.
pub const DEFAULT_PRELOAD: &str = concat!(
    "json,erb,pathname,fileutils,time,date,digest,base64,",
    "ostruct,set,stringio,logger,socket"
);



pub fn normalize_preload(v: &str) -> String {
    if v == "0" || v == "none" {
        String::new()
    } else {
        v.to_string()
    }
}



/// Sobe de um dir ate a raiz procurando um arquivo (mesma busca do Bundler).
pub fn find_in_parents_from(base: &Path, name: &str) -> Option<PathBuf> {
    let mut dir: Option<&Path> = Some(base);
    while let Some(d) = dir {
        let cand = d.join(name);
        if cand.is_file() {
            return Some(cand);
        }
        dir = d.parent();
    }
    None
}



/// Sobe do cwd ate a raiz procurando um arquivo (mesma busca do Bundler).
pub fn find_in_parents(name: &str) -> Option<PathBuf> {
    let cwd = env::current_dir().ok()?;
    find_in_parents_from(&cwd, name)
}



/// .env (Fase E): carrega o primeiro `.env` subindo do cwd, sem sobrescrever
/// vars ja definidas (semantica dotenv). Roda no CLIENTE: o env resultante
/// propaga para o spawn do daemon (o boot da app ve DATABASE_URL etc.), para
/// o env_blob do RUN (o script ve) e para o modo --cold (paridade cold/warm
/// preservada — um parser so no daemon divergiria o cold).
pub fn load_dotenv() {
    let Some(file) = find_in_parents(".env") else { return };
    let Ok(content) = fs::read_to_string(&file) else { return };
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim();
        let Some((k, v)) = line.split_once('=') else { continue };
        let k = k.trim();
        if k.is_empty() || env::var_os(k).is_some() {
            continue;
        }
        let mut v = v.trim().to_string();
        if v.len() >= 2 {
            let q = v.as_bytes()[0] as char;
            if (q == '"' || q == '\'') && v.ends_with(q) {
                v = v[1..v.len() - 1].to_string();
            }
        }
        env::set_var(k, v);
    }
}



/// Um Gemfile no cwd (ou BUNDLE_GEMFILE) significa que o Bundler.setup do
/// child vai ativar as gems do app — preload de stdlib nao pode coexistir:
/// se o Gemfile pina uma default gem ja preloaded (ex.: base64 0.2 vs 0.3 do
/// Sinatra 4), o Bundler aborta com "already activated". Com Gemfile, o
/// preload fica vazio e o bundler ativa o necessario (interpretador "fresco",
/// como o `bundle exec`).
/// Gemfile/gems.rb a partir de um dir explicito (ou BUNDLE_GEMFILE). O
/// child (pos-chdir) e o cold (cwd proprio) usam a base certa — o env do
/// processo cliente pode nao ser o do alvo.
pub fn has_gemfile_from(base: &Path) -> bool {
    if env::var_os("BUNDLE_GEMFILE").is_some() {
        return true;
    }
    find_in_parents_from(base, "Gemfile").is_some()
        || find_in_parents_from(base, "gems.rb").is_some()
}



pub fn has_gemfile() -> bool {
    let Ok(cwd) = env::current_dir() else { return false };
    has_gemfile_from(&cwd)
}


#[derive(Debug, Clone)]
pub struct AppConfig {
    // campos publicos: os comandos (run/test/task/serve/exec/repl/doctor)
    // leem root/preload/scripts diretamente
    /// dir do calisto.toml (raiz da app; o daemon roda com este cwd)
    pub root: PathBuf,
    /// entrypoint a pre-carregar no daemon (ex.: config/environment.rb)
    pub preload: Option<PathBuf>,
    /// `[run] compact` (Fase M): compacta o heap pos-boot (GC.start +
    /// GC.compact) antes de aceitar conexoes. None = default (on quando ha
    /// preload, ou seja, daemon de app). Flag de performance — nao entra no
    /// hash do socket (mudar nao reinicia o daemon, como scripts).
    pub compact: Option<bool>,
    /// `[run] yjit` (Fase N): daemon da app boota com `--yjit` — o warmup
    /// compila o hot path no daemon e cada child nasce com o codigo JIT
    /// pronto (paginas CoW). None = off.
    pub yjit: Option<bool>,
    /// `[run] warmup` (Fase N): script rodado no daemon pos-boot (depois do
    /// preload da app, antes da compactacao) — ex.: N requests contra a
    /// Rack app em memoria. Responsabilidade da app; falha avisa e segue.
    pub warmup: Option<PathBuf>,
    /// `[scripts]` nome -> comando (ordem do arquivo; Fase H)
    pub scripts: Vec<(String, String)>,
}



/// `[run] compact` efetivo: CALISTO_COMPACT (override de operacao/testes) >
/// calisto.toml > default on quando ha preload (daemon de app). O daemon
/// compacta o heap apos o boot — os children (fork) compartilham quase todas
/// as paginas via CoW (Fase M.1).
pub fn app_compact(app: &AppConfig) -> Result<bool, String> {
    if let Ok(v) = env::var("CALISTO_COMPACT") {
        return match v.as_str() {
            "1" | "true" => Ok(true),
            "0" | "false" => Ok(false),
            _ => Err(format!("CALISTO_COMPACT deve ser 0/1/true/false (achei '{v}')")),
        };
    }
    Ok(app.compact.unwrap_or(app.preload.is_some()))
}



/// `[run] yjit` efetivo (Fase N): CALISTO_YJIT (override) > calisto.toml >
/// off. Com yjit, o daemon da app boota com `--yjit` e o warmup compila o
/// hot path antes do accept loop — os children herdam o codigo compilado.
pub fn app_yjit(app: &AppConfig) -> Result<bool, String> {
    if let Ok(v) = env::var("CALISTO_YJIT") {
        return match v.as_str() {
            "1" | "true" => Ok(true),
            "0" | "false" => Ok(false),
            _ => Err(format!("CALISTO_YJIT deve ser 0/1/true/false (achei '{v}')")),
        };
    }
    Ok(app.yjit.unwrap_or(false))
}



/// `[run] warmup` efetivo (Fase N): CALISTO_WARMUP (override; `0`/`none`
/// desliga) > calisto.toml > nenhum. Caminho relativo ao calisto.toml, como
/// o preload.
pub fn app_warmup(app: &AppConfig) -> Result<Option<PathBuf>, String> {
    if let Ok(v) = env::var("CALISTO_WARMUP") {
        return Ok(match v.as_str() {
            "0" | "none" => None,
            p if !p.is_empty() => Some(PathBuf::from(p)),
            _ => None,
        });
    }
    Ok(app.warmup.clone())
}

impl AppConfig {
    /// argv do comando de `[scripts.NAME]` (tokenizado shell-like, sem
    /// escapes/expansao). Ok(None) = script nao definido; Err = comando
    /// invalido (vazio ou aspas desbalanceadas).
    pub fn script_command(&self, name: &str) -> Result<Option<Vec<String>>, String> {
        let Some((_, cmd)) = self.scripts.iter().find(|(n, _)| n == name) else {
            return Ok(None);
        };
        let argv = split_command(cmd)?;
        if argv.is_empty() {
            return Err(format!("calisto.toml: script '{name}' sem comando"));
        }
        Ok(Some(argv))
    }
}



/// Tokeniza o comando de um `[scripts]` (shell-like minimo): whitespace
/// separa palavras e aspas simples/duplas agrupam — sem escapes, sem
/// expansao de variaveis (subset do TOML que o parser ja usa).
pub fn split_command(s: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    for c in s.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => cur.push(c),
            None => match c {
                '\'' | '"' => quote = Some(c),
                c if c.is_whitespace() => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                c => cur.push(c),
            },
        }
    }
    if let Some(q) = quote {
        return Err(format!("calisto.toml: aspas '{q}' nao fechadas em \"{s}\""));
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}



pub fn parse_calisto_toml(content: &str, base: &Path) -> Result<AppConfig, String> {
    let mut preload: Option<PathBuf> = None;
    let mut compact: Option<bool> = None;
    let mut yjit: Option<bool> = None;
    let mut warmup: Option<PathBuf> = None;
    let mut scripts: Vec<(String, String)> = Vec::new();
    let mut section = "run"; // chave sem secao = [run] (backwards compat)
    for (i, raw) in content.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            let sec = line.trim_start_matches('[').trim_end_matches(']').trim();
            if sec != "run" && sec != "scripts" {
                return Err(format!("calisto.toml:{}: secao desconhecida '{sec}'", i + 1));
            }
            section = sec;
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            return Err(format!("calisto.toml:{}: linha invalida: {raw}", i + 1));
        };
        let k = k.trim();
        // chaves booleanas do [run] aceitam TOML puro (true/false sem aspas),
        // como no TOML de verdade; o resto do subset exige "valor" entre aspas
        let bare = k == "compact" || k == "yjit";
        let value = match v.trim().strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            Some(s) => s.to_string(),
            None if bare => v.trim().to_string(),
            None => {
                return Err(format!("calisto.toml:{}: {k} precisa ser \"valor\"", i + 1));
            }
        };
        match section {
            "run" => match k {
                "preload" => {
                    if value.is_empty() {
                        return Err(format!("calisto.toml:{}: preload vazio", i + 1));
                    }
                    preload = Some(base.join(&value));
                }
                "compact" => match value.as_str() {
                    "true" => compact = Some(true),
                    "false" => compact = Some(false),
                    _ => {
                        return Err(format!(
                            "calisto.toml:{}: compact precisa ser true/false (achei '{value}')",
                            i + 1
                        ));
                    }
                },
                "yjit" => match value.as_str() {
                    "true" => yjit = Some(true),
                    "false" => yjit = Some(false),
                    _ => {
                        return Err(format!(
                            "calisto.toml:{}: yjit precisa ser true/false (achei '{value}')",
                            i + 1
                        ));
                    }
                },
                "warmup" => {
                    if value.is_empty() {
                        return Err(format!("calisto.toml:{}: warmup vazio", i + 1));
                    }
                    warmup = Some(base.join(&value));
                }
                _ => {
                    return Err(format!(
                        "calisto.toml:{}: chave desconhecida '{k}' (scripts vao na secao [scripts])",
                        i + 1
                    ));
                }
            },
            _ => {
                // [scripts]: nome -> comando (validado na resolucao do run)
                scripts.push((k.to_string(), value.to_string()));
            }
        }
    }
    if let Some(p) = &preload {
        if !p.is_file() {
            return Err(format!(
                "calisto.toml: preload '{}' nao existe",
                p.display()
            ));
        }
    }
    if let Some(w) = &warmup {
        if !w.is_file() {
            return Err(format!(
                "calisto.toml: warmup '{}' nao existe",
                w.display()
            ));
        }
    }
    Ok(AppConfig { root: base.to_path_buf(), preload, compact, yjit, warmup, scripts })
}



/// Detecta app do cwd (walk up, como Gemfile). Erro de parse e estrito no
/// `run`; status/stop/doctor tratam como sem-app com warning.
pub fn load_app_config() -> Result<Option<AppConfig>, String> {
    let Some(file) = find_in_parents("calisto.toml") else {
        return Ok(None);
    };
    let content = fs::read_to_string(&file)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    let app = parse_calisto_toml(&content, file.parent().unwrap_or(Path::new(".")))?;
    Ok(Some(app))
}



/// FNV-1a 64 — hash estavel sem dep para isolar o daemon de cada app.
pub fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}



/// Daemon dedicado por app (como Spring/Zeus): o preload de app vive no boot
/// do daemon, entao ele so serve aquela app. O hash inclui o entrypoint para
/// mudancas de calisto.toml ganharem socket novo, e um sal para separar
/// ambientes (ex.: dev vs teste — o daemon de teste boota com RAILS_ENV=test).
pub fn app_runtime_dir_for(app: &AppConfig, salt: &str, ruby: &Path) -> PathBuf {
    let preload = app
        .preload
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let base = format!("{}\0{}\0{}", app.root.display(), preload, salt);
    // versao no hash: daemons da app de VMs diferentes convivem. O pin default
    // mantem o hash classico (sem quebrar sockets existentes).
    let key = match ruby_version_of(ruby) {
        Some(v) => format!("{base}\0{v}"),
        None => base,
    };
    runtime_dir().join("apps").join(format!("{:016x}", fnv1a(&key)))
}



pub fn app_runtime_dir(app: &AppConfig, ruby: &Path) -> PathBuf {
    app_runtime_dir_for(app, "", ruby)
}



/// Daemon de teste: igual ao da app, mas boota com RAILS_ENV=test/RACK_ENV=test
/// (o Rails fixa o env no boot; um fork do boot dev nunca enxergaria :test).
pub fn app_test_runtime_dir(app: &AppConfig, ruby: &Path) -> PathBuf {
    app_runtime_dir_for(app, "test", ruby)
}



/// App com daemon dedicado: so quando ha entrypoint para pre-carregar (Fase
/// B). Um calisto.toml so com `[scripts]` (Fase H) nao justifica daemon da
/// app — os comandos rodam no daemon generico (preload default).
pub fn app_daemon(app: &Option<AppConfig>) -> Option<&AppConfig> {
    app.as_ref().filter(|a| a.preload.is_some())
}



// ---- Fase E: calisto task ----------------------------------------------------

/// Preload padrao para daemons nao-app: vazio com Gemfile (bundler ativa as
/// gems; preload colidiria), senao CALISTO_PRELOAD ou o default.
pub fn run_preload(app: &Option<AppConfig>) -> String {
    if app_daemon(app).is_some() {
        String::new() // app: o entrypoint e o preload (Fase B)
    } else if has_gemfile() {
        String::new()
    } else {
        env::var("CALISTO_PRELOAD")
            .map_or_else(|_| DEFAULT_PRELOAD.to_string(), |v| normalize_preload(&v))
    }
}
