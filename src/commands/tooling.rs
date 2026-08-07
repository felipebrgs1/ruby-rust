//! calisto — tooling
//!
//! calisto init/upgrade/completions.
//! Extraido de src/main.rs na reorganizacao do CLI (estrutura inspirada no cli/ do Deno).
//! calisto — commands/tooling (extraido de src/main.rs na reorg do CLI).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use crate::runtime::*;







// ---- Fase J: calisto init ----------------------------------------------------

pub const INIT_CALISTO_TOML: &str = "# calisto.toml — config da app calisto (subset minimo de TOML).
# Scripts rodam com `calisto run <nome>`; um arquivo com o mesmo nome sempre
# vence (ex.: `calisto run hello.rb` roda o arquivo, nao o script).
# Sem [run] preload o daemon e o generico (preload stdlib). Quando a app tiver
# um boot pesado (Rails etc.), adicione:
#   [run]
#   preload = \"config/environment.rb\"
# e o daemon da app congela o boot (cada comando roda como fork do boot).
[scripts]
start = \"./hello.rb\"
";



pub const INIT_HELLO_RB: &str = "#!/usr/bin/env ruby
# frozen_string_literal: true
# Ola do calisto! Edite este arquivo e rode `calisto run` (ou `calisto run
# hello.rb`) — o daemon quente roda no interpretador pre-carregado.
puts \"Hello from calisto!\"
";



pub const INIT_GITIGNORE: &str = "/.bundle/
/vendor/bundle/
";



/// `calisto init [name] [--force]` — scaffold de app (como `bun init`):
/// calisto.toml com `[scripts] start = \"./hello.rb\"`, hello.rb e .gitignore.
/// Sem nome usa o diretorio atual; `calisto init <name>` cria `<name>/`
/// (erro se for arquivo). Nunca sobrescreve arquivo existente sem --force.
/// O app gerado roda com `calisto run` (bare = o script `start`).
pub fn cmd_init(args: &[String]) -> i32 {
    let mut name: Option<&str> = None;
    let mut force = false;
    for a in args {
        match a.as_str() {
            "--force" | "-f" => force = true,
            s if s.starts_with('-') => {
                eprintln!("calisto: flag desconhecida '{s}' (calisto init [name] [--force])");
                return 1;
            }
            s => {
                if name.is_some() {
                    eprintln!("calisto: argumento inesperado '{s}'");
                    return 1;
                }
                name = Some(s);
            }
        }
    }
    let target = match name {
        Some(n) => {
            let p = PathBuf::from(n);
            if p.is_file() {
                eprintln!("calisto: init: '{n}' e um arquivo, nao um diretorio");
                return 1;
            }
            p
        }
        None => PathBuf::from("."),
    };
    if let Err(e) = fs::create_dir_all(&target) {
        eprintln!("calisto: init: cannot create {}: {e}", target.display());
        return 1;
    }
    // hello.rb precisa de shebang + +x: o shim do exec resolve executaveis
    // (caminho de arquivo -> spec do bundle -> PATH) e detecta binario ruby
    // pelo shebang para `load` in-process — o script do scaffold nao depende
    // de ruby no PATH.
    for (file, content, exec) in [
        ("calisto.toml", INIT_CALISTO_TOML, false),
        ("hello.rb", INIT_HELLO_RB, true),
        (".gitignore", INIT_GITIGNORE, false),
    ] {
        let path = target.join(file);
        if path.exists() && !force {
            eprintln!(
                "calisto: init: {} ja existe (use --force para sobrescrever)",
                path.display()
            );
            return 1;
        }
        if let Err(e) = fs::write(&path, content) {
            eprintln!("calisto: init: cannot write {}: {e}", path.display());
            return 1;
        }
        if exec {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o755));
        }
    }
    let rel = if name.is_some() {
        format!("./{}", target.display())
    } else {
        ".".to_string()
    };
    println!("Initialized calisto app in {rel}");
    println!("next: cd {rel} && calisto run");
    0
}



// ---- Fase J: calisto upgrade -------------------------------------------------

/// Versoes com sha256 conhecido no scripts/build-ruby.sh (espelho do case do
/// script). O CLI valida ANTES de spawnar: uma versao desconhecida falharia
/// so dentro do script com "unbound variable" (set -u), sem sha pra verificar.
pub const KNOWN_RUBY_VERSIONS: &[&str] = &["3.4.10", "3.4.4"];



/// `calisto upgrade [version] [--source]`:
/// - no checkout (scripts/build-ruby.sh presente): roda o script — sem
///   versao rebuilda o pin (vendor/current); com versao constroi
///   vendor/ruby-<v> (Fase I). Idempotente: o script pula rubies ja
///   construidos (verifica o sha do tarball baixado antes).
/// - instalacao portatil (Fase Q.2 — CALISTO_HOME do curl|sh; o tarball nao
///   traz scripts/): BAIXA o ruby pre-compilado do release em vez de
///   compilar (curl + tar + sha256 — tools do sistema, zero deps).
/// `--source` forca o build pelo script (erro claro se ausente).
/// `CALISTO_BUILD_SCRIPT` (testes) aponta outro script; o caminho real e
/// resolvido junto ao vendor (subindo do binario). `CALISTO_UPGRADE_URL`
/// (testes) troca a base dos downloads pre-compilados (qualquer URL que o
/// curl entenda, ex.: file://).
pub fn cmd_upgrade(args: &[String]) -> i32 {
    let mut source_only = false;
    let mut version: Option<&str> = None;
    for a in args {
        match a.as_str() {
            "--source" => source_only = true,
            v if v.starts_with('-') => {
                eprintln!("calisto: upgrade: flag desconhecida '{v}'");
                return 1;
            }
            v => {
                if version.is_some() {
                    eprintln!(
                        "calisto: upgrade precisa de no maximo um argumento: calisto upgrade [version] [--source]"
                    );
                    return 1;
                }
                version = Some(v);
            }
        }
    }
    if let Some(v) = version {
        if !KNOWN_RUBY_VERSIONS.contains(&v) {
            eprintln!(
                "calisto: upgrade: versao {v} sem sha256 conhecido ({}); \
                 rode RUBY_SHA256=<sha> scripts/build-ruby.sh manualmente",
                KNOWN_RUBY_VERSIONS.join("/")
            );
            return 1;
        }
    }
    let script_env = env::var_os("CALISTO_BUILD_SCRIPT").map(PathBuf::from);
    let script = match &script_env {
        Some(p) => Some(p.clone()),
        None => vendor_root().map(|vendor| vendor.join("../scripts/build-ruby.sh")),
    };
    let script_exists = script.as_deref().is_some_and(|s| s.is_file());
    if source_only && !script_exists {
        eprintln!(
            "calisto: upgrade: --source pediu o build, mas {} nao existe \
             (instalacao portatil; use upgrade sem --source para baixar)",
            script
                .as_deref()
                .map(|s| s.display().to_string())
                .unwrap_or_else(|| "<sem vendor>".into())
        );
        return 1;
    }
    if script_exists {
        let script = script.unwrap();
        let mut cmd = Command::new("sh");
        cmd.arg(&script).stdin(Stdio::null());
        if let Some(v) = version {
            cmd.env("RUBY_VERSION", v);
        }
        return match cmd.status() {
            // stdio herdado: o build (10-15min) mostra o progresso ao vivo
            Ok(st) => st.code().unwrap_or(1),
            Err(e) => {
                eprintln!("calisto: upgrade: cannot run {}: {e}", script.display());
                1
            }
        };
    }
    // override EXPLICITO ausente = erro (o caller pediu esse script);
    // resolucao por vendor ausente (instalacao portatil Fase Q.2) = download
    if script_env.is_some() {
        eprintln!(
            "calisto: upgrade: {} nao encontrado (rode scripts/build-ruby.sh manualmente)",
            script.as_deref().unwrap().display()
        );
        return 1;
    }
    upgrade_download(version)
}



/// Baixa um ruby pre-compilado do release (Fase Q.2). Layout do tarball:
/// `ruby-<v>/...` extraido em <vendor>/ (CALISTO_HOME/vendor na instalacao
/// portatil). Verifica sha256 contra o arquivo `.sha256` publicado junto.
pub fn upgrade_download(version: Option<&str>) -> i32 {
    let Some(vendor) = vendor_root() else {
        eprintln!("calisto: upgrade: vendor/ nao encontrado (rode do checkout do calisto)");
        return 1;
    };
    let v = version.unwrap_or(PINNED_RUBY);
    let base = env::var("CALISTO_UPGRADE_URL").unwrap_or_else(|_| {
        format!(
            "https://github.com/felipebrgs1/ruby-rust/releases/download/v{}",
            env!("CARGO_PKG_VERSION")
        )
    });
    let url = format!("{base}/calisto-ruby-{v}-linux-x86_64.tar.gz");
    let tmp = env::temp_dir().join(format!("calisto-upgrade-{}", std::process::id()));
    let _ = fs::create_dir_all(&tmp);
    // o nome do arquivo baixado = basename da URL: o .sha256 publicado
    // referencia esse nome (sha256sum -c valida contra ele)
    let name = url.rsplit('/').next().unwrap_or("ruby.tar.gz");
    let tarball = tmp.join(name);
    let sha_file = tmp.join(format!("{name}.sha256"));
    eprintln!("calisto: upgrade: baixando {url}");
    let curl = |out: &Path, u: &str| {
        Command::new("curl")
            .args(["-fsSL", "-o"])
            .arg(out)
            .arg(u)
            .stdin(Stdio::null())
            .status()
    };
    if curl(&tarball, &url).map(|s| !s.success()).unwrap_or(true)
        || curl(&sha_file, &format!("{url}.sha256"))
            .map(|s| !s.success())
            .unwrap_or(true)
    {
        eprintln!("calisto: upgrade: download falhou ({url}; verifique a rede)");
        let _ = fs::remove_dir_all(&tmp);
        return 1;
    }
    // sha256 -c: o arquivo .sha256 contem "<hash>  <nome>" (release.sh)
    let mut check = Command::new("sha256sum");
    check
        .arg("-c")
        .arg(&sha_file)
        .current_dir(&tmp)
        .stdin(Stdio::null());
    let ok = check.status().map(|s| s.success()).unwrap_or(false);
    if !ok {
        eprintln!("calisto: upgrade: sha256 do download nao confere (abortando)");
        let _ = fs::remove_dir_all(&tmp);
        return 1;
    }
    let _ = fs::create_dir_all(&vendor);
    let st = Command::new("tar")
        .arg("-xzf")
        .arg(&tarball)
        .arg("-C")
        .arg(&vendor)
        .status();
    let _ = fs::remove_dir_all(&tmp);
    match st {
        Ok(s) if s.success() => {
            eprintln!(
                "calisto: upgrade: ruby {v} instalado em {}",
                vendor.join(format!("ruby-{v}")).display()
            );
            0
        }
        Ok(_) => {
            eprintln!("calisto: upgrade: extracao do tarball falhou");
            1
        }
        Err(e) => {
            eprintln!("calisto: upgrade: cannot run tar: {e}");
            1
        }
    }
}



// ---- Fase J: calisto completions ---------------------------------------------

pub const BASH_COMPLETION: &str = r#"# bash completion for calisto
# Instale com: calisto completions bash > /etc/bash_completion.d/calisto
_calisto() {
    local cur
    cur="${COMP_WORDS[COMP_CWORD]}"
    local commands="run test task serve exec repl build init upgrade completions add remove lock status stop doctor help"
    if (( COMP_CWORD == 1 )); then
        COMPREPLY=( $(compgen -W "${commands}" -- "${cur}") )
        return 0
    fi
    case "${COMP_WORDS[1]}" in
        run)
            if [[ "${cur}" == -* ]]; then
                COMPREPLY=( $(compgen -W "--cold --time --preload -e --eval" -- "${cur}") )
            else
                COMPREPLY=( $(compgen -f -X '!*.rb' -- "${cur}") )
            fi
            ;;
        test)
            if [[ "${cur}" == -* ]]; then
                COMPREPLY=( $(compgen -W "--watch" -- "${cur}") )
            else
                COMPREPLY=( $(compgen -f -- "${cur}") )
            fi
            ;;
        serve)
            [[ "${cur}" == -* ]] && COMPREPLY=( $(compgen -W "-p --port -o --host" -- "${cur}") )
            ;;
        build)
            if [[ "${cur}" == -* ]]; then
                COMPREPLY=( $(compgen -W "--compile -o --out --root" -- "${cur}") )
            else
                COMPREPLY=( $(compgen -f -X '!*.rb' -- "${cur}") )
            fi
            ;;
        init)
            if [[ "${cur}" == -* ]]; then
                COMPREPLY=( $(compgen -W "--force -f" -- "${cur}") )
            else
                COMPREPLY=( $(compgen -d -- "${cur}") )
            fi
            ;;
        upgrade)
            COMPREPLY=( $(compgen -W "3.4.10 3.4.4" -- "${cur}") )
            ;;
        completions)
            COMPREPLY=( $(compgen -W "bash zsh" -- "${cur}") )
            ;;
    esac
}
complete -F _calisto calisto
"#;



pub const ZSH_COMPLETION: &str = r#"#compdef calisto
# zsh completion for calisto
# Instale com: calisto completions zsh > ~/.zfunc/_calisto
_calisto() {
    local -a commands
    commands=(
        'run:executa script/script do calisto.toml no daemon quente'
        'test:roda a suite (minitest/rspec) no daemon quente'
        'task:rake no daemon quente'
        'serve:sobe a Rack app do config.ru'
        'exec:binario de uma gem no contexto da app'
        'repl:IRB no contexto da app'
        'build:empacota a app num arquivo unico'
        'init:scaffold de app (calisto.toml + hello.rb)'
        'upgrade:rebuild do pin / build de versao'
        'completions:gera completions (bash/zsh)'
        'add:adiciona gem ao Gemfile (bundle add)'
        'remove:remove gem do Gemfile (bundle remove)'
        'lock:atualiza o Gemfile.lock (bundle lock)'
        'status:estado do daemon'
        'stop:para o daemon'
        'doctor:diagnostico do ambiente'
        'help:ajuda'
    )
    if (( CURRENT == 2 )); then
        _describe 'command' commands
        return
    fi
    case "${words[2]}" in
        run)
            if [[ "$PREFIX" == -* ]]; then
                _arguments '--cold' '--time' '--preload[lista de stdlib]' '-e[codigo inline]' '--eval' '*:arquivo:_files -g "*.rb"'
            else
                _files -g '*.rb'
            fi
            ;;
        test) _arguments '--watch[re-roda ao salvar]' '*:arquivo:_files' ;;
        serve) _arguments '-p[porta]' '--port[porta]' '-o[host]' '--host[host]' ;;
        build) _arguments '--compile[embute gems pure-Ruby]' '-o[saida]' '--out[saida]' '--root[raiz do projeto]' '*:arquivo:_files -g "*.rb"' ;;
        init) _arguments '--force[sobrescreve arquivos]' '-f[sobrescreve arquivos]' '*:diretorio:_files -/' ;;
        upgrade) _values 'versao' 3.4.10 3.4.4 ;;
        completions) _values 'shell' bash zsh ;;
    esac
}
compdef _calisto calisto
"#;



/// `calisto completions <bash|zsh>` — imprime o script de completions do
/// shell em stdout (redirecione para o arquivo de completions). Sem shell ou
/// shell desconhecido: erro de uso.
pub fn cmd_completions(args: &[String]) -> i32 {
    match args {
        [shell] if shell == "bash" => {
            print!("{BASH_COMPLETION}");
            0
        }
        [shell] if shell == "zsh" => {
            print!("{ZSH_COMPLETION}");
            0
        }
        [other] => {
            eprintln!("calisto: completions: shell desconhecido '{other}' (bash|zsh)");
            1
        }
        _ => {
            eprintln!("calisto: uso: calisto completions <bash|zsh>");
            1
        }
    }
}
