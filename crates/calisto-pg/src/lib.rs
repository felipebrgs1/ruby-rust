//! calisto-pg — camada de compatibilidade com a gem `pg`.
//!
//! FFI hand-rolled (zero crates, convencao do repo) sobre a **libpq** via
//! dlopen — mesmo padrao do calisto-sqlite (libsqlite3) e do calisto-ruby
//! (libruby). O modulo `PG` (Connection/Result como TypedData) e registrado
//! no boot do daemon (rb_define_method via calisto-ruby) e os children do
//! fork herdam as classes.
//!
//! Escopo (surface do ActiveRecord 7/8, aterrado no postgresql_adapter.rb do
//! Rails 8.1): `PG.connect`/`Connection#new` (conninfo string ou Hash),
//! exec/exec_params/query/prepare/exec_prepared, alias async_* (no pg 1.x
//! sao sincronos — aqui tambem), escape*/parameter_status/transaction_status/
//! reset/cancel/socket_io/set_client_encoding, e `PG::Result` com
//! values/fields/ntuples/nfields/[]/getvalue/getisnull/ftype/fmod/cmd_tuples/
//! clear/map_types!. O resto do surface (constantes, type maps/decoders,
//! PG::Tuple, `each`/`each_row`) vive no shim `gems/pg.rb` — o typecast do
//! proprio AR e a fonte da verdade; os maps sao aceitos e armazenados, mas a
//! conversao nativa devolve strings (correto, so menos otimizado que o pg
//! com BasicTypeMapForResults).
//!
//! Resolucao da libpq (ordem): `CALISTO_LIBPQ` (env — client/testes),
//! `libpq.so.5`/`libpq.so` do sistema, e o libpq **vendored** em
//! `<vendor>/src/postgresql-*/install/lib` (walk-up do ruby resolvido via
//! `CALISTO_EMBED_RUBY` — o caso deste dev, onde nao existe libpq de
//! sistema). Sem lib acha: o daemon degrada com warning e o shim do require
//! levanta LoadError claro (como o sqlite).

use calisto_ruby::{RbDataType, RbDataTypeFunction, Ruby, VALUE};
use std::ffi::{c_char, c_int, c_long, c_uint, c_void, CStr, CString};
use std::sync::Mutex;

// ---- libpq (C API estavel; assinaturas conferidas no libpq-fe.h 16.6) ------

#[repr(C)]
struct PGconn {
    _private: [u8; 0],
}
#[repr(C)]
struct PGresult {
    _private: [u8; 0],
}
#[repr(C)]
struct PGcancel {
    _private: [u8; 0],
}
/// Layout do libpq-fe.h (conferido no 16.6): 6 char* + dispsize int. A
/// lista termina na entrada com keyword NULL; `val` pode ser NULL.
#[repr(C)]
struct PQconninfoOption {
    keyword: *mut c_char,
    envvar: *mut c_char,
    compiled: *mut c_char,
    val: *mut c_char,
    label: *mut c_char,
    dispchar: *mut c_char,
    dispsize: c_int,
}
type Oid = c_uint;

const CONNECTION_OK: c_int = 0;
const PGRES_FATAL_ERROR: c_int = 7;

struct PqFns {
    connectdb: unsafe extern "C" fn(*const c_char) -> *mut PGconn,
    status: unsafe extern "C" fn(*const PGconn) -> c_int,
    error_message: unsafe extern "C" fn(*const PGconn) -> *const c_char,
    finish: unsafe extern "C" fn(*mut PGconn),
    db: unsafe extern "C" fn(*const PGconn) -> *mut c_char,
    user: unsafe extern "C" fn(*const PGconn) -> *mut c_char,
    host: unsafe extern "C" fn(*const PGconn) -> *mut c_char,
    port: unsafe extern "C" fn(*const PGconn) -> *mut c_char,
    parameter_status: unsafe extern "C" fn(*const PGconn, *const c_char) -> *const c_char,
    server_version: unsafe extern "C" fn(*const PGconn) -> c_int,
    transaction_status: unsafe extern "C" fn(*const PGconn) -> c_int,
    set_client_encoding: unsafe extern "C" fn(*mut PGconn, *const c_char) -> c_int,
    reset: unsafe extern "C" fn(*mut PGconn),
    exec: unsafe extern "C" fn(*mut PGconn, *const c_char) -> *mut PGresult,
    exec_params: unsafe extern "C" fn(
        *mut PGconn,
        *const c_char,
        c_int,
        *const Oid,
        *const *const c_char,
        *const c_int,
        *const c_int,
        c_int,
    ) -> *mut PGresult,
    prepare: unsafe extern "C" fn(*mut PGconn, *const c_char, *const c_char, c_int, *const Oid) -> *mut PGresult,
    exec_prepared: unsafe extern "C" fn(
        *mut PGconn,
        *const c_char,
        c_int,
        *const *const c_char,
        *const c_int,
        *const c_int,
        c_int,
    ) -> *mut PGresult,
    result_status: unsafe extern "C" fn(*const PGresult) -> c_int,
    result_error_message: unsafe extern "C" fn(*const PGresult) -> *const c_char,
    ntuples: unsafe extern "C" fn(*const PGresult) -> c_int,
    nfields: unsafe extern "C" fn(*const PGresult) -> c_int,
    fname: unsafe extern "C" fn(*const PGresult, c_int) -> *mut c_char,
    ftype: unsafe extern "C" fn(*const PGresult, c_int) -> Oid,
    fmod: unsafe extern "C" fn(*const PGresult, c_int) -> c_int,
    getlength: unsafe extern "C" fn(*const PGresult, c_int, c_int) -> c_int,
    getvalue: unsafe extern "C" fn(*const PGresult, c_int, c_int) -> *mut c_char,
    getisnull: unsafe extern "C" fn(*const PGresult, c_int, c_int) -> c_int,
    cmd_tuples: unsafe extern "C" fn(*mut PGresult) -> *mut c_char,
    clear: unsafe extern "C" fn(*mut PGresult),
    freemem: unsafe extern "C" fn(*mut c_void),
    escape_string_conn: unsafe extern "C" fn(*mut PGconn, *mut c_char, *const c_char, usize, *mut c_int) -> usize,
    escape_literal: unsafe extern "C" fn(*mut PGconn, *const c_char, usize) -> *mut c_char,
    escape_identifier: unsafe extern "C" fn(*mut PGconn, *const c_char, usize) -> *mut c_char,
    lib_version: unsafe extern "C" fn() -> c_int,
    socket: unsafe extern "C" fn(*const PGconn) -> c_int,
    get_cancel: unsafe extern "C" fn(*const PGconn) -> *mut PGcancel,
    cancel: unsafe extern "C" fn(*mut PGcancel, *mut c_char, c_int) -> c_int,
    free_cancel: unsafe extern "C" fn(*mut PGcancel),
    conndefaults: unsafe extern "C" fn() -> *mut PQconninfoOption,
    get_result: unsafe extern "C" fn(*mut PGconn) -> *mut PGresult,
}

fn last_dlerror() -> String {
    let p = unsafe { CStr::from_ptr(dlerror()) };
    p.to_string_lossy().into_owned()
}

unsafe extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *mut c_char;
}

unsafe fn load_sym<T>(handle: *mut c_void, name: &'static [u8]) -> Result<T, String> {
    let sym = dlsym(handle, name.as_ptr() as *const c_char);
    if sym.is_null() {
        return Err(format!(
            "simbolo '{}' nao encontrado na libpq: {}",
            String::from_utf8_lossy(&name[..name.len() - 1]),
            last_dlerror()
        ));
    }
    Ok(std::mem::transmute_copy(&sym))
}

/// Caminhos candidatos da libpq. Ordem: `CALISTO_LIBPQ` (override hermetico
/// de client/testes), SONAME do sistema, e o libpq vendored do checkout
/// (`<vendor>/src/postgresql-*/install/lib/libpq.so.5` — derivado do ruby
/// resolvido via CALISTO_EMBED_RUBY, que o client seta no spawn do daemon).
fn pq_candidates() -> Vec<CString> {
    let mut out = Vec::new();
    if let Ok(p) = std::env::var("CALISTO_LIBPQ") {
        if !p.is_empty() {
            out.push(CString::new(p).unwrap_or_default());
        }
    }
    for name in ["libpq.so.5", "libpq.so"] {
        out.push(CString::new(name).unwrap());
    }
    // vendored: <ruby>/../../../src/postgresql-*/install/lib
    if let Ok(ruby) = std::env::var("CALISTO_EMBED_RUBY") {
        let rb = std::path::PathBuf::from(&ruby);
        if let Some(vendor) = rb.parent().and_then(|p| p.parent()).and_then(|p| p.parent()) {
            let src = vendor.join("src");
            if let Ok(rd) = std::fs::read_dir(&src) {
                let mut names: Vec<_> = rd
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with("postgresql-"))
                    .collect();
                names.sort();
                for n in names {
                    let so = src.join(&n).join("install").join("lib").join("libpq.so.5");
                    if so.is_file() {
                        out.push(CString::new(so.to_string_lossy().into_owned()).unwrap());
                    }
                }
            }
        }
    }
    out
}

/// dlopen da libpq (candidatos em ordem). Err claro se nenhum existir — o
/// daemon degrada (avisa e segue; o shim do require levanta LoadError).
fn pq_open_lib() -> Result<PqFns, String> {
    let mut handle: *mut c_void = std::ptr::null_mut();
    for c in pq_candidates() {
        handle = unsafe { dlopen(c.as_ptr(), 2) }; // RTLD_NOW
        if !handle.is_null() {
            break;
        }
    }
    if handle.is_null() {
        return Err(format!("dlopen libpq: {}", last_dlerror()));
    }
    unsafe {
        Ok(PqFns {
            connectdb: load_sym(handle, b"PQconnectdb\0")?,
            status: load_sym(handle, b"PQstatus\0")?,
            error_message: load_sym(handle, b"PQerrorMessage\0")?,
            finish: load_sym(handle, b"PQfinish\0")?,
            db: load_sym(handle, b"PQdb\0")?,
            user: load_sym(handle, b"PQuser\0")?,
            host: load_sym(handle, b"PQhost\0")?,
            port: load_sym(handle, b"PQport\0")?,
            parameter_status: load_sym(handle, b"PQparameterStatus\0")?,
            server_version: load_sym(handle, b"PQserverVersion\0")?,
            transaction_status: load_sym(handle, b"PQtransactionStatus\0")?,
            set_client_encoding: load_sym(handle, b"PQsetClientEncoding\0")?,
            reset: load_sym(handle, b"PQreset\0")?,
            exec: load_sym(handle, b"PQexec\0")?,
            exec_params: load_sym(handle, b"PQexecParams\0")?,
            prepare: load_sym(handle, b"PQprepare\0")?,
            exec_prepared: load_sym(handle, b"PQexecPrepared\0")?,
            result_status: load_sym(handle, b"PQresultStatus\0")?,
            result_error_message: load_sym(handle, b"PQresultErrorMessage\0")?,
            ntuples: load_sym(handle, b"PQntuples\0")?,
            nfields: load_sym(handle, b"PQnfields\0")?,
            fname: load_sym(handle, b"PQfname\0")?,
            ftype: load_sym(handle, b"PQftype\0")?,
            fmod: load_sym(handle, b"PQfmod\0")?,
            getlength: load_sym(handle, b"PQgetlength\0")?,
            getvalue: load_sym(handle, b"PQgetvalue\0")?,
            getisnull: load_sym(handle, b"PQgetisnull\0")?,
            cmd_tuples: load_sym(handle, b"PQcmdTuples\0")?,
            clear: load_sym(handle, b"PQclear\0")?,
            freemem: load_sym(handle, b"PQfreemem\0")?,
            escape_string_conn: load_sym(handle, b"PQescapeStringConn\0")?,
            escape_literal: load_sym(handle, b"PQescapeLiteral\0")?,
            escape_identifier: load_sym(handle, b"PQescapeIdentifier\0")?,
            lib_version: load_sym(handle, b"PQlibVersion\0")?,
            socket: load_sym(handle, b"PQsocket\0")?,
            get_cancel: load_sym(handle, b"PQgetCancel\0")?,
            cancel: load_sym(handle, b"PQcancel\0")?,
            free_cancel: load_sym(handle, b"PQfreeCancel\0")?,
            conndefaults: load_sym(handle, b"PQconndefaults\0")?,
            get_result: load_sym(handle, b"PQgetResult\0")?,
        })
    }
}

// ---- VM + classes registradas -------------------------------------------------

static VM: Mutex<Option<usize>> = Mutex::new(None);
/// PqFns num Box (heap) — o ponteiro do stack do register() morreria.
static PQ: Mutex<Option<Box<PqFns>>> = Mutex::new(None);
static CLASS_ERROR: Mutex<Option<VALUE>> = Mutex::new(None);
static CLASS_CONNECTION_BAD: Mutex<Option<VALUE>> = Mutex::new(None);
static CLASS_CONNECTION: Mutex<Option<VALUE>> = Mutex::new(None);
static CLASS_RESULT: Mutex<Option<VALUE>> = Mutex::new(None);

fn vm() -> &'static Ruby {
    let g = VM.lock().unwrap();
    unsafe { &*(g.expect("calisto-pg: register() nao chamado") as *const Ruby) }
}

fn pq() -> &'static PqFns {
    let g = PQ.lock().unwrap();
    let b = g.as_ref().expect("calisto-pg: register() nao chamado");
    let p: *const PqFns = &**b;
    unsafe { &*p }
}

fn error_class() -> VALUE {
    CLASS_ERROR.lock().unwrap().expect("error class")
}

fn connection_bad_class() -> VALUE {
    CLASS_CONNECTION_BAD.lock().unwrap().expect("connection bad class")
}

fn connection_class() -> VALUE {
    CLASS_CONNECTION.lock().unwrap().expect("connection class")
}

fn result_class() -> VALUE {
    CLASS_RESULT.lock().unwrap().expect("result class")
}

/// Indice UTF-8 da VM (rb_utf8_encindex — constante). LazyLock: inicializa
/// no primeiro acesso (pos-register, quando a VM esta bootada) e depois e
/// um load atomico — um Mutex aqui custaria ~20-40ns POR STRING.
static UTF8_ENC_IDX: std::sync::LazyLock<c_int> =
    std::sync::LazyLock::new(|| unsafe { (vm().native().rb_utf8_encindex)() });

fn utf8_enc_idx() -> c_int {
    *UTF8_ENC_IDX
}

/// String Ruby de bytes com tag UTF-8 — caminho rapido igual ao
/// pg_text_dec_string do pg gem: rb_str_new + associar o INDICE de encoding
/// (rb_enc_associate_index, lookup em array). O rb_utf8_str_new da API faz
/// rb_enc_to_index (lookup na tabela de encodings) POR STRING — ~100ns a
/// mais por célula (o gap de 2x medido na conversao de 200k linhas).
fn utf8_string(vm: &Ruby, ptr: *const c_char, len: c_long) -> VALUE {
    let n = vm.native();
    let s = unsafe { (n.rb_str_new)(ptr, len) };
    unsafe { (n.rb_enc_associate_index)(s, utf8_enc_idx()) };
    s
}

// ---- TypedData (handles conn/result com dfree no GC) --------------------------

/// Ponteiro cru num static exige Sync — o conteudo nunca e mutado depois da
/// inicializacao const (wrap_struct_name/functions/parent sao estaticos), o
/// unsafe impl e sound.
struct SyncPtr<T>(T);
unsafe impl<T> Sync for SyncPtr<T> {}

unsafe extern "C" fn conn_free(p: *mut c_void) {
    if !p.is_null() {
        let h = p as *mut Conn;
        let conn = unsafe { (*h).conn };
        if !conn.is_null() {
            let s = pq();
            unsafe { (s.finish)(conn) };
        }
        unsafe { drop(Box::from_raw(h)) };
    }
}

unsafe extern "C" fn res_free(p: *mut c_void) {
    if !p.is_null() {
        let h = p as *mut Res;
        let res = unsafe { (*h).res };
        if !res.is_null() {
            let s = pq();
            unsafe { (s.clear)(res) };
        }
        unsafe { drop(Box::from_raw(h)) };
    }
}

/// Alloc do padrao de C extension: o CRuby avisa "undefining the allocator
/// of T_DATA class" sem um alloc proprio (rb_data_object_check do gc.c). O
/// box ja nasce aqui (conn nulo = fechado) — `initialize` so preenche.
unsafe extern "C" fn conn_alloc(klass: VALUE) -> VALUE {
    let boxed = Box::into_raw(Box::new(Conn {
        conn: std::ptr::null_mut(),
        decode: false,
    }));
    (vm().native().rb_data_typed_object_wrap)(klass, boxed as *mut c_void, conn_type())
}

unsafe extern "C" fn res_alloc(klass: VALUE) -> VALUE {
    let boxed = Box::into_raw(Box::new(Res {
        res: std::ptr::null_mut(),
        decode: false,
    }));
    (vm().native().rb_data_typed_object_wrap)(klass, boxed as *mut c_void, res_type())
}

static CONN_TYPE: SyncPtr<RbDataType> = SyncPtr(RbDataType {
    wrap_struct_name: c"PG::Connection".as_ptr(),
    function: RbDataTypeFunction {
        dmark: None,
        dfree: Some(conn_free),
        dsize: None,
        dcompact: None,
        reserved: [std::ptr::null_mut()],
    },
    parent: std::ptr::null(),
    data: std::ptr::null_mut(),
    flags: calisto_ruby::RUBY_TYPED_FREE_IMMEDIATELY,
});

static RES_TYPE: SyncPtr<RbDataType> = SyncPtr(RbDataType {
    wrap_struct_name: c"PG::Result".as_ptr(),
    function: RbDataTypeFunction {
        dmark: None,
        dfree: Some(res_free),
        dsize: None,
        dcompact: None,
        reserved: [std::ptr::null_mut()],
    },
    parent: std::ptr::null(),
    data: std::ptr::null_mut(),
    flags: calisto_ruby::RUBY_TYPED_FREE_IMMEDIATELY,
});

fn conn_type() -> &'static RbDataType {
    &CONN_TYPE.0
}

fn res_type() -> &'static RbDataType {
    &RES_TYPE.0
}

/// Handle de Connection: `conn` nulo = fechado (close/finish/GC); `decode`
/// = type_map_for_results setado (conversao de valores por OID — espelho do
/// pg gem, que herda o type map do conn nos results).
#[repr(C)]
struct Conn {
    conn: *mut PGconn,
    decode: bool,
}

/// Handle de Result: `res` nulo = limpo (clear/GC); `decode` herdado da
/// conexao no momento da criacao (como o p_typemap do pg gem). Resultados
/// sao autossuficientes em memoria (PQclear e seguro apos PQfinish).
#[repr(C)]
struct Res {
    res: *mut PGresult,
    decode: bool,
}

fn conn_of(vm: &Ruby, obj: VALUE) -> *mut Conn {
    let p = unsafe { (vm.native().rb_check_typeddata)(obj, conn_type()) };
    if p.is_null() {
        vm.raise(error_class(), "Connection sem handle");
    }
    p as *mut Conn
}

fn res_of(vm: &Ruby, obj: VALUE) -> *mut Res {
    let p = unsafe { (vm.native().rb_check_typeddata)(obj, res_type()) };
    if p.is_null() {
        vm.raise(error_class(), "Result sem handle");
    }
    p as *mut Res
}

fn check_open_conn(vm: &Ruby, h: *mut Conn) -> *mut PGconn {
    let conn = unsafe { (*h).conn };
    if conn.is_null() {
        vm.raise(error_class(), "connection is closed");
    }
    conn
}

fn check_open_res(vm: &Ruby, h: *mut Res) -> *mut PGresult {
    let res = unsafe { (*h).res };
    if res.is_null() {
        vm.raise(error_class(), "result is cleared");
    }
    res
}

// ---- helpers ---------------------------------------------------------------

/// Erro do conn (PQerrorMessage), sem o trailing newline do libpq.
fn conn_error_msg(p: &PqFns, conn: *mut PGconn) -> String {
    let msg = unsafe { CStr::from_ptr((p.error_message)(conn)) };
    msg.to_string_lossy().trim_end().to_string()
}

/// Mensagem de erro de um PGresult (PQresultErrorMessage), trimada.
fn result_error_msg(p: &PqFns, res: *mut PGresult) -> String {
    let msg = unsafe { CStr::from_ptr((p.result_error_message)(res)) };
    msg.to_string_lossy().trim_end().to_string()
}

/// String Ruby -> CString (NUL interior -> ArgumentError, como StringValueCStr).
fn cstr_arg(vm: &Ruby, v: VALUE, what: &str) -> CString {
    let mut sv = v;
    let (ptr, len) = vm.string_bytes(&mut sv);
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    match CString::new(slice) {
        Ok(c) => c,
        Err(_) => vm.raise(vm.e_arg_error(), &format!("{what} nao pode conter NUL")),
    }
}

/// Hash de conninfo -> string `k=v ...` no formato do libpq. Valores com
/// espaco/aspas/backslash sao quotados com aspas simples (escape \ e ').
fn conninfo_from_value(vm: &Ruby, v: VALUE) -> CString {
    let n = vm.native();
    if vm.is_kind_of(v, vm.c_string()) {
        return cstr_arg(vm, v, "conninfo");
    }
    let hash_class = vm.const_get("Hash");
    if !vm.is_kind_of(v, hash_class) {
        vm.raise(
            vm.e_type_error(),
            &format!("expected String or Hash of connection params, got {}", vm.classname(v)),
        );
    }
    let pairs = vm.funcall(v, "to_a", &[]);
    let count = unsafe { (n.rb_num2ll)(vm.funcall(pairs, "length", &[])) };
    let mut out = String::new();
    for i in 0..count {
        let pair = unsafe { (n.rb_ary_entry)(pairs, i as c_long) };
        let k = vm.funcall(unsafe { (n.rb_ary_entry)(pair, 0) }, "to_s", &[]);
        let val = vm.funcall(unsafe { (n.rb_ary_entry)(pair, 1) }, "to_s", &[]);
        let mut ksv = k;
        let (kptr, klen) = vm.string_bytes(&mut ksv);
        let kstr = String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(kptr, klen) }).into_owned();
        let mut vsv = val;
        let (vptr, vlen) = vm.string_bytes(&mut vsv);
        let vstr = String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(vptr, vlen) }).into_owned();
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&kstr);
        out.push('=');
        out.push_str(&conninfo_quote(&vstr));
    }
    CString::new(out).unwrap_or_else(|_| CString::new("").unwrap())
}

fn conninfo_quote(v: &str) -> String {
    let needs = v.is_empty()
        || v.chars()
            .any(|c| matches!(c, ' ' | '\'' | '"' | '\\') || c.is_control());
    if !needs {
        return v.to_string();
    }
    let mut out = String::with_capacity(v.len() + 2);
    out.push('\'');
    for c in v.chars() {
        if c == '\'' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// Converte o array de binds (nil/String, ou qualquer tipo via to_s — o AR
/// passa o que o type_map_for_queries encoderia) em CStrings. nil -> slot
/// NULL do libpq. Os CStrings ficam VIVOS no struct (drop no caller, depois
/// da chamada libpq — devolver so os ponteiros deixaria dangling).
struct Params {
    _keep: Vec<Option<CString>>,
    ptrs: Vec<*const c_char>,
}

impl Params {
    fn len(&self) -> c_int {
        self.ptrs.len() as c_int
    }

    fn ptrs(&self) -> *const *const c_char {
        if self.ptrs.is_empty() {
            std::ptr::null()
        } else {
            self.ptrs.as_ptr()
        }
    }
}

fn bind_params(vm: &Ruby, v: VALUE) -> Params {
    let n = vm.native();
    if v == calisto_ruby::Qnil {
        return Params {
            _keep: Vec::new(),
            ptrs: Vec::new(),
        };
    }
    let array_class = vm.const_get("Array");
    if !vm.is_kind_of(v, array_class) {
        vm.raise(
            vm.e_type_error(),
            &format!("expected Array of params, got {}", vm.classname(v)),
        );
    }
    let count = unsafe { (n.rb_num2ll)(vm.funcall(v, "length", &[])) };
    let mut keep: Vec<Option<CString>> = Vec::with_capacity(count as usize);
    for i in 0..count {
        let el = unsafe { (n.rb_ary_entry)(v, i as c_long) };
        if el == calisto_ruby::Qnil {
            keep.push(None);
        } else {
            let s = if vm.is_kind_of(el, vm.c_string()) {
                el
            } else {
                vm.funcall(el, "to_s", &[])
            };
            let mut sv = s;
            let (ptr, len) = vm.string_bytes(&mut sv);
            let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
            match CString::new(slice) {
                Ok(c) => keep.push(Some(c)),
                Err(_) => vm.raise(vm.e_arg_error(), "parametro nao pode conter NUL"),
            }
        }
    }
    let ptrs = keep
        .iter()
        .map(|c| match c {
            Some(s) => s.as_ptr(),
            None => std::ptr::null(),
        })
        .collect();
    Params { _keep: keep, ptrs }
}

/// Erro fatal de um PGresult -> mensagem (o CALLER decide o clear/raise).
fn fatal_message(p: &PqFns, res: *mut PGresult) -> Option<String> {
    if unsafe { (p.result_status)(res) } == PGRES_FATAL_ERROR {
        Some(result_error_msg(p, res))
    } else {
        None
    }
}

/// Empacota um PGresult como PG::Result (TypedData) — o dfree limpa no GC.
/// `decode` herda o flag do Connection (type_map_for_results do pg gem).
fn wrap_result(vm: &Ruby, res: *mut PGresult, decode: bool) -> VALUE {
    let boxed = Box::into_raw(Box::new(Res { res, decode }));
    unsafe { (vm.native().rb_data_typed_object_wrap)(result_class(), boxed as *mut c_void, res_type()) }
}

/// Parse de int texto canonico do postgres (digitos ASCII, sinal opcional) —
/// a mao, como o fast path do pg_text_dec_integer (o pg documenta o
/// rb_cstr2inum como lento; from_utf8+parse do std e pior ainda: validacao
/// UTF-8 + aritmetica checked por digito). Sem overflow check: o texto do
/// int8 cobre exatamente o range do i64 (o pg tambem nao checa).
fn parse_i64_raw(c: *const c_char, len: c_int) -> Option<i64> {
    if len <= 0 {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(c as *const u8, len as usize) };
    let (neg, start) = match bytes[0] {
        b'-' => (true, 1),
        b'+' => (false, 1),
        b'0'..=b'9' => (false, 0),
        _ => return None,
    };
    if start >= bytes.len() {
        return None;
    }
    let mut i: i64 = 0;
    for &b in &bytes[start..] {
        match b {
            b'0'..=b'9' => i = i * 10 + (b - b'0') as i64,
            _ => return None,
        }
    }
    Some(if neg { -i } else { i })
}

/// Decodifica uma celula por OID (espelho do type map do AR/pg gem):
/// int2/4/8/oid -> Integer (rb_ll2inum — Fixnum, SEM alocacao de string),
/// float4/8 -> Float (NaN/Infinity do postgres mapeados), bool -> true/false.
/// OIDs fora do conjunto -> None (o caller cai na string; AR typecasta
/// strings corretamente — date/timestamp/numeric/bytea ficam string).
fn decode_value(vm: &Ruby, oid: Oid, c: *const c_char, len: c_int) -> Option<VALUE> {
    match oid {
        20 | 21 | 23 | 26 => {
            // int8/int2/int4/oid
            parse_i64_raw(c, len).map(|i| unsafe { (vm.native().rb_ll2inum)(i) })
        }
        700 | 701 => {
            // float4/float8 — formatos do postgres: "NaN", "Infinity",
            // "-Infinity", ou numero canonico
            let bytes = unsafe { std::slice::from_raw_parts(c as *const u8, len as usize) };
            let v = match bytes {
                b"NaN" => f64::NAN,
                b"Infinity" => f64::INFINITY,
                b"-Infinity" => f64::NEG_INFINITY,
                _ => std::str::from_utf8(bytes).ok()?.parse::<f64>().ok()?,
            };
            Some(unsafe { (vm.native().rb_float_new)(v) })
        }
        16 => match unsafe { *c as u8 } {
            // bool: formato texto do libpq ('t'/'f')
            b't' => Some(calisto_ruby::Qtrue),
            b'f' => Some(calisto_ruby::Qfalse),
            _ => None,
        },
        _ => None,
    }
}

/// Linha i do resultado como Array (nil para NULL; com decode=true, valores
/// tipados por OID — int/bool/float — senao strings).
fn row_values(vm: &Ruby, res: *mut PGresult, i: c_int, decode: bool) -> VALUE {
    let p = pq();
    let n = vm.native();
    let nf = unsafe { (p.nfields)(res) };
    // OIDs por coluna UMA vez (o PQftype e leitura de campo no libpq, mas
    // evita a chamada por celula no loop quente)
    let oids: Vec<Oid> = if decode {
        (0..nf).map(|j| unsafe { (p.ftype)(res, j) }).collect()
    } else {
        Vec::new()
    };
    // mesmo padrao do pgresult_values do pg gem, com UMA diferenca critica:
    // as células sao empurradas num array Ruby criado ANTES do loop (nao num
    // Vec Rust + rb_ary_new_from_values no fim). O array e um root visivel
    // ao GC conservador (esta no stack Rust) — um GC disparado por uma
    // alocacao mid-loop coleta as strings que so viveriam no buffer do Vec
    // (invisivel), deixando ponteiros dangling no array final. (Bug real
    // achado em GC: "try to mark T_NONE object".)
    let row = unsafe { (n.rb_ary_new_capa)(nf as c_long) };
    for j in 0..nf {
        let v = if unsafe { (p.getisnull)(res, i, j) } != 0 {
            calisto_ruby::Qnil
        } else {
            let c = unsafe { (p.getvalue)(res, i, j) };
            let len = unsafe { (p.getlength)(res, i, j) };
            if decode {
                match decode_value(vm, oids[j as usize], c, len) {
                    Some(v) => v,
                    None => utf8_string(vm, c, len as c_long),
                }
            } else {
                utf8_string(vm, c, len as c_long)
            }
        };
        unsafe { (n.rb_ary_push)(row, v) };
    }
    row
}

// ---- metodos nativos: PG::Connection -----------------------------------------

/// `PG::Connection.new(conninfo_string | hash)` — AR chama `PG.connect(**params)`.
unsafe extern "C" fn conn_initialize(argc: c_int, argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc > 1 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0..1)"));
    }
    let h = conn_of(vm, self_);
    let p = pq();
    // re-initialize: fecha a conexao anterior antes de reconectar
    let old = unsafe { (*h).conn };
    if !old.is_null() {
        (p.finish)(old);
        unsafe { (*h).conn = std::ptr::null_mut() };
    }
    let conninfo = if argc == 0 {
        CString::new("").unwrap()
    } else {
        conninfo_from_value(vm, *argv)
    };
    let conn = (p.connectdb)(conninfo.as_ptr());
    if conn.is_null() {
        vm.raise(connection_bad_class(), "PQconnectdb devolveu NULL (sem memoria?)");
    }
    // guarda ANTES do status check — o dfree do GC fecha se o raise abaixo
    // descartar o objeto
    unsafe { (*h).conn = conn };
    if (p.status)(conn) != CONNECTION_OK {
        let msg = conn_error_msg(p, conn);
        vm.raise(connection_bad_class(), &msg);
    }
    self_
}

/// `Connection#exec(sql)` — sync; levanta PG::Error em erro fatal.
unsafe extern "C" fn conn_exec(argc: c_int, argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 1 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 1)"));
    }
    let h = conn_of(vm, self_);
    let conn = check_open_conn(vm, h);
    let sql = cstr_arg(vm, *argv, "SQL");
    let p = pq();
    let res = (p.exec)(conn, sql.as_ptr());
    if res.is_null() {
        vm.raise(error_class(), "PQexec devolveu NULL (sem memoria?)");
    }
    if let Some(msg) = fatal_message(p, res) {
        (p.clear)(res);
        vm.raise(error_class(), &msg);
    }
    wrap_result(vm, res, (*h).decode)
}

/// `Connection#exec_params(sql, params=nil)` — binds nil/String (ou to_s).
unsafe extern "C" fn conn_exec_params(argc: c_int, argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 1 && argc != 2 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 1..2)"));
    }
    let h = conn_of(vm, self_);
    let conn = check_open_conn(vm, h);
    let sql = cstr_arg(vm, *argv, "SQL");
    let params = if argc == 2 { bind_params(vm, *argv.add(1)) } else { Params { _keep: Vec::new(), ptrs: Vec::new() } };
    let p = pq();
    let res = (p.exec_params)(conn, sql.as_ptr(), params.len(), std::ptr::null(), params.ptrs(), std::ptr::null(), std::ptr::null(), 0);
    if res.is_null() {
        vm.raise(error_class(), "PQexecParams devolveu NULL (sem memoria?)");
    }
    if let Some(msg) = fatal_message(p, res) {
        (p.clear)(res);
        vm.raise(error_class(), &msg);
    }
    wrap_result(vm, res, (*h).decode)
}

/// `Connection#prepare(name, sql)` — statement nomeada na sessao.
unsafe extern "C" fn conn_prepare(argc: c_int, argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 2 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 2)"));
    }
    let h = conn_of(vm, self_);
    let conn = check_open_conn(vm, h);
    let name = cstr_arg(vm, *argv, "name");
    let sql = cstr_arg(vm, *argv.add(1), "SQL");
    let p = pq();
    let res = (p.prepare)(conn, name.as_ptr(), sql.as_ptr(), 0, std::ptr::null());
    if res.is_null() {
        vm.raise(error_class(), "PQprepare devolveu NULL (sem memoria?)");
    }
    if let Some(msg) = fatal_message(p, res) {
        (p.clear)(res);
        vm.raise(error_class(), &msg);
    }
    (p.clear)(res);
    calisto_ruby::Qnil
}

/// `Connection#exec_prepared(name, params=nil)`.
unsafe extern "C" fn conn_exec_prepared(argc: c_int, argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 1 && argc != 2 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 1..2)"));
    }
    let h = conn_of(vm, self_);
    let conn = check_open_conn(vm, h);
    let name = cstr_arg(vm, *argv, "name");
    let params = if argc == 2 { bind_params(vm, *argv.add(1)) } else { Params { _keep: Vec::new(), ptrs: Vec::new() } };
    let p = pq();
    let res = (p.exec_prepared)(conn, name.as_ptr(), params.len(), params.ptrs(), std::ptr::null(), std::ptr::null(), 0);
    if res.is_null() {
        vm.raise(error_class(), "PQexecPrepared devolveu NULL (sem memoria?)");
    }
    if let Some(msg) = fatal_message(p, res) {
        (p.clear)(res);
        vm.raise(error_class(), &msg);
    }
    wrap_result(vm, res, (*h).decode)
}

/// `Connection#escape(str)` — PQescapeStringConn (AR: quote_string).
unsafe extern "C" fn conn_escape(argc: c_int, argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 1 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 1)"));
    }
    let h = conn_of(vm, self_);
    let conn = check_open_conn(vm, h);
    let mut sv = unsafe { *argv };
    let (ptr, len) = vm.string_bytes(&mut sv);
    let p = pq();
    let mut buf = vec![0u8; len * 2 + 1];
    let mut err: c_int = 0;
    let out_len = unsafe {
        (p.escape_string_conn)(
            conn,
            buf.as_mut_ptr() as *mut c_char,
            ptr as *const c_char,
            len,
            &mut err,
        )
    };
    if err != 0 {
        vm.raise(error_class(), &format!("PQescapeStringConn falhou: {}", conn_error_msg(p, conn)));
    }
    utf8_string(vm, buf.as_ptr() as *const c_char, out_len as c_long)
}

/// `Connection#escape_literal(str)` / `#escape_identifier(str)` — retorno
/// mallocado do libpq, copiado e liberado.
unsafe extern "C" fn conn_escape_literal(argc: c_int, argv: *mut VALUE, self_: VALUE) -> VALUE {
    escape_alloc(argc, argv, self_, true)
}

unsafe extern "C" fn conn_escape_identifier(argc: c_int, argv: *mut VALUE, self_: VALUE) -> VALUE {
    escape_alloc(argc, argv, self_, false)
}

fn escape_alloc(argc: c_int, argv: *mut VALUE, self_: VALUE, literal: bool) -> VALUE {
    let vm = vm();
    if argc != 1 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 1)"));
    }
    let h = conn_of(vm, self_);
    let conn = check_open_conn(vm, h);
    let mut sv = unsafe { *argv };
    let (ptr, len) = vm.string_bytes(&mut sv);
    let p = pq();
    let out = unsafe {
        if literal {
            (p.escape_literal)(conn, ptr as *const c_char, len)
        } else {
            (p.escape_identifier)(conn, ptr as *const c_char, len)
        }
    };
    if out.is_null() {
        vm.raise(error_class(), "escape devolveu NULL (sem memoria?)");
    }
    let s = unsafe { CStr::from_ptr(out) }.to_bytes();
    let v = vm.utf8_str(s);
    unsafe { (p.freemem)(out as *mut c_void) };
    v
}

/// `Connection#parameter_status(name)` -> String | nil.
unsafe extern "C" fn conn_parameter_status(argc: c_int, argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 1 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 1)"));
    }
    let h = conn_of(vm, self_);
    let conn = check_open_conn(vm, h);
    let name = cstr_arg(vm, *argv, "parameter name");
    let p = pq();
    let s = (p.parameter_status)(conn, name.as_ptr());
    if s.is_null() {
        calisto_ruby::Qnil
    } else {
        vm.utf8_str(unsafe { CStr::from_ptr(s) }.to_bytes())
    }
}

/// `Connection#server_version` -> int
unsafe extern "C" fn conn_server_version(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = conn_of(vm, self_);
    let conn = check_open_conn(vm, h);
    (vm.native().rb_ll2inum)((pq().server_version)(conn) as i64)
}

/// `Connection#transaction_status` -> int (PG::PQTRANS_*)
unsafe extern "C" fn conn_transaction_status(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = conn_of(vm, self_);
    let conn = check_open_conn(vm, h);
    (vm.native().rb_ll2inum)((pq().transaction_status)(conn) as i64)
}

/// `Connection#status` -> int (PG::CONNECTION_OK = 0)
unsafe extern "C" fn conn_status(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = conn_of(vm, self_);
    let conn = check_open_conn(vm, h);
    (vm.native().rb_ll2inum)((pq().status)(conn) as i64)
}

/// `Connection#db` / `#user` / `#host` / `#port` — atributos do conninfo.
/// Auxiliar com seletor (0=db,1=user,2=host,3=port).
fn conn_attr_of(vm: &Ruby, self_: VALUE, sel: u8) -> VALUE {
    let h = conn_of(vm, self_);
    let conn = check_open_conn(vm, h);
    let p = pq();
    let s = unsafe {
        match sel {
            0 => (p.db)(conn),
            1 => (p.user)(conn),
            2 => (p.host)(conn),
            _ => (p.port)(conn),
        }
    };
    if s.is_null() {
        vm.str("")
    } else {
        vm.utf8_str(unsafe { CStr::from_ptr(s) }.to_bytes())
    }
}

unsafe extern "C" fn conn_db(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    conn_attr_of(vm, self_, 0)
}

unsafe extern "C" fn conn_user(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    conn_attr_of(vm, self_, 1)
}

unsafe extern "C" fn conn_host(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    conn_attr_of(vm, self_, 2)
}

unsafe extern "C" fn conn_port(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    conn_attr_of(vm, self_, 3)
}

/// `Connection#close` / `#finish` — PQfinish; `#closed?` / `#finished?`.
unsafe extern "C" fn conn_close(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = conn_of(vm, self_);
    let conn = unsafe { (*h).conn };
    if !conn.is_null() {
        (pq().finish)(conn);
        unsafe { (*h).conn = std::ptr::null_mut() };
    }
    calisto_ruby::Qnil
}

unsafe extern "C" fn conn_closed(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = conn_of(vm, self_);
    if unsafe { (*h).conn.is_null() } {
        calisto_ruby::Qtrue
    } else {
        calisto_ruby::Qfalse
    }
}

/// `Connection#reset` — reconecta no mesmo conninfo (AR: reconnect).
unsafe extern "C" fn conn_reset(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = conn_of(vm, self_);
    let conn = check_open_conn(vm, h);
    let p = pq();
    (p.reset)(conn);
    if (p.status)(conn) != CONNECTION_OK {
        let msg = conn_error_msg(p, conn);
        vm.raise(connection_bad_class(), &msg);
    }
    self_
}

/// `Connection#cancel` — cancela a query em curso (buffer de erro do libpq).
unsafe extern "C" fn conn_cancel(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = conn_of(vm, self_);
    let conn = check_open_conn(vm, h);
    let p = pq();
    let cancel = (p.get_cancel)(conn);
    if cancel.is_null() {
        vm.raise(error_class(), "PQgetCancel devolveu NULL (sem memoria?)");
    }
    let mut errbuf = [0u8; 256];
    let rc = (p.cancel)(cancel, errbuf.as_mut_ptr() as *mut c_char, errbuf.len() as c_int);
    (p.free_cancel)(cancel);
    if rc == 0 {
        let msg = CStr::from_bytes_until_nul(&errbuf).unwrap_or_default();
        vm.raise(error_class(), &format!("cancel falhou: {}", msg.to_string_lossy()));
    }
    calisto_ruby::Qnil
}

/// `Connection#block` — no-op (async_* sao sincronos); devolve self.
unsafe extern "C" fn conn_block(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    self_
}

/// `Connection#socket_io` — IO em volta do fd do socket (AR faz reopen(IO::NULL)).
unsafe extern "C" fn conn_socket_io(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = conn_of(vm, self_);
    let conn = check_open_conn(vm, h);
    let fd = (pq().socket)(conn);
    if fd < 0 {
        vm.raise(error_class(), "PQsocket devolveu fd invalido");
    }
    let fd_v = (vm.native().rb_ll2inum)(fd as i64);
    vm.funcall(vm.const_get("IO"), "for_fd", &[fd_v])
}

/// `Connection#set_client_encoding(name)` — AR: configure_connection.
unsafe extern "C" fn conn_set_client_encoding(argc: c_int, argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 1 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 1)"));
    }
    let h = conn_of(vm, self_);
    let conn = check_open_conn(vm, h);
    let enc = cstr_arg(vm, *argv, "encoding");
    let p = pq();
    let rc = (p.set_client_encoding)(conn, enc.as_ptr());
    if rc != 0 {
        vm.raise(error_class(), &format!("set_client_encoding falhou: {}", conn_error_msg(p, conn)));
    }
    calisto_ruby::Qnil
}

/// `Connection#set_notice_receiver(&block)` — aceita e ignora por enquanto
/// (degradacao documentada: NOTICEs do servidor nao sao entregues; o AR so
/// usa para logar warnings quando db_warnings_action esta configurado).
unsafe extern "C" fn conn_set_notice_receiver(argc: c_int, _argv: *mut VALUE, _self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    calisto_ruby::Qnil
}

/// `Connection#_calisto_result_type_map!(map)` — privado (shim): marca se a
/// conversao de valores decodifica por OID (o equivalente funcional do
/// `type_map_for_results=` do pg gem; o map em si e guardado como ivar no
/// shim). Nil desliga; qualquer outro valor liga.
unsafe extern "C" fn conn_calisto_result_type_map(argc: c_int, argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 1 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 1)"));
    }
    let h = conn_of(vm, self_);
    unsafe { (*h).decode = *argv != calisto_ruby::Qnil };
    *argv
}

/// `Connection#get_last_result` — consome um resultado pendente da fila do
/// libpq (o AR 7.1 chama apos `prepare` para "limpar a fila"). No modelo
/// sync nao ha resultado pendente -> nil (como o pg gem em conexao sync).
unsafe extern "C" fn conn_get_last_result(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = conn_of(vm, self_);
    let conn = check_open_conn(vm, h);
    let p = pq();
    let res = (p.get_result)(conn);
    if res.is_null() {
        return calisto_ruby::Qnil;
    }
    if let Some(msg) = fatal_message(p, res) {
        (p.clear)(res);
        vm.raise(error_class(), &msg);
    }
    wrap_result(vm, res, (*h).decode)
}

/// `PG::Connection.conndefaults_hash` — Hash { simbolo => valor } dos
/// defaults do libpq (o AR 7.2 valida os params do config com
/// `conn_params.slice!(*PG::Connection.conndefaults_hash.keys)` — chaves
/// precisam ser Symbol, como o pg gem).
unsafe extern "C" fn conn_conndefaults_hash(argc: c_int, _argv: *mut VALUE, _self: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let p = pq();
    let opts = (p.conndefaults)();
    if opts.is_null() {
        vm.raise(error_class(), "PQconndefaults devolveu NULL");
    }
    let n = vm.native();
    let hash = vm.funcall(vm.const_get("Hash"), "new", &[]);
    let mut i = 0usize;
    loop {
        let opt = unsafe { opts.add(i) };
        if unsafe { (*opt).keyword.is_null() } {
            break;
        }
        let keyword = unsafe { CStr::from_ptr((*opt).keyword) }.to_string_lossy().into_owned();
        let sym = unsafe { (n.rb_id2sym)(vm.intern(&keyword)) };
        let val = if unsafe { (*opt).val.is_null() } {
            calisto_ruby::Qnil
        } else {
            vm.str(&unsafe { CStr::from_ptr((*opt).val) }.to_string_lossy())
        };
        vm.funcall(hash, "[]=", &[sym, val]);
        i += 1;
    }
    hash
}

/// `PG::Connection.quote_ident(name)` — class method (AR 7.2 usa no
/// quoting de identificadores): aspas duplas com `"` duplicado.
unsafe extern "C" fn conn_quote_ident(argc: c_int, argv: *mut VALUE, _self: VALUE) -> VALUE {
    let vm = vm();
    if argc != 1 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 1)"));
    }
    let mut sv = *argv;
    let (ptr, len) = vm.string_bytes(&mut sv);
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len) };
    let mut out = Vec::with_capacity(len + 2);
    out.push(b'"');
    for &b in bytes {
        if b == b'"' {
            out.push(b'"');
        }
        out.push(b);
    }
    out.push(b'"');
    vm.utf8_str(&out)
}

// ---- metodos nativos: PG::Result ---------------------------------------------

/// `Result#values` — Array de linhas (nil para NULL; com type map setado
/// na conexao, valores tipados por OID: int/bool/float — senao strings).
unsafe extern "C" fn res_values(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = res_of(vm, self_);
    let res = check_open_res(vm, h);
    let decode = unsafe { (*h).decode };
    let p = pq();
    let n = vm.native();
    let nt = (p.ntuples)(res);
    let rows = unsafe { (n.rb_ary_new_capa)(nt as c_long) };
    for i in 0..nt {
        unsafe { (n.rb_ary_push)(rows, row_values(vm, res, i, decode)) };
    }
    rows
}

/// `Result#fields` — nomes das colunas.
unsafe extern "C" fn res_fields(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = res_of(vm, self_);
    let res = check_open_res(vm, h);
    let p = pq();
    let n = vm.native();
    let nf = (p.nfields)(res);
    let fields = unsafe { (n.rb_ary_new_capa)(nf as c_long) };
    for j in 0..nf {
        let name = (p.fname)(res, j);
        let bytes = unsafe { CStr::from_ptr(name) }.to_bytes();
        let s = utf8_string(vm, bytes.as_ptr() as *const c_char, bytes.len() as c_long);
        unsafe { (n.rb_ary_push)(fields, s) };
    }
    fields
}

/// `Result#ntuples` / `#nfields`
unsafe extern "C" fn res_ntuples(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = res_of(vm, self_);
    let res = check_open_res(vm, h);
    (vm.native().rb_ll2inum)((pq().ntuples)(res) as i64)
}

unsafe extern "C" fn res_nfields(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = res_of(vm, self_);
    let res = check_open_res(vm, h);
    (vm.native().rb_ll2inum)((pq().nfields)(res) as i64)
}

/// `Result#[](index)` — linha como Array (nil fora do range).
unsafe extern "C" fn res_get(argc: c_int, argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 1 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 1)"));
    }
    let h = res_of(vm, self_);
    let res = check_open_res(vm, h);
    let i = (vm.native().rb_num2ll)(*argv);
    let p = pq();
    if i < 0 || i >= (p.ntuples)(res) as i64 {
        return calisto_ruby::Qnil;
    }
    row_values(vm, res, i as c_int, unsafe { (*h).decode })
}

/// `Result#getvalue(row, col)` / `#getisnull(row, col)` — bounds checados.
unsafe extern "C" fn res_getvalue(argc: c_int, argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 2 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 2)"));
    }
    let h = res_of(vm, self_);
    let res = check_open_res(vm, h);
    let p = pq();
    let i = (vm.native().rb_num2ll)(*argv);
    let j = (vm.native().rb_num2ll)(*argv.add(1));
    if i < 0 || i >= (p.ntuples)(res) as i64 || j < 0 || j >= (p.nfields)(res) as i64 {
        vm.raise(vm.e_arg_error(), "indice fora do range do result");
    }
    if (p.getisnull)(res, i as c_int, j as c_int) != 0 {
        return calisto_ruby::Qnil;
    }
    let c = (p.getvalue)(res, i as c_int, j as c_int);
    let len = (p.getlength)(res, i as c_int, j as c_int);
    utf8_string(vm, c, len as c_long)
}

/// `Result#typed_getvalue(row, col)` — como getvalue mas decodifica por OID
/// quando o result tem type map (usado pelo PG::Tuple do shim — o pg 1.5+
/// devolve valores tipados nas tuplas). nil para NULL.
unsafe extern "C" fn res_typed_getvalue(argc: c_int, argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 2 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 2)"));
    }
    let h = res_of(vm, self_);
    let res = check_open_res(vm, h);
    let p = pq();
    let i = (vm.native().rb_num2ll)(*argv);
    let j = (vm.native().rb_num2ll)(*argv.add(1));
    if i < 0 || i >= (p.ntuples)(res) as i64 || j < 0 || j >= (p.nfields)(res) as i64 {
        vm.raise(vm.e_arg_error(), "indice fora do range do result");
    }
    if (p.getisnull)(res, i as c_int, j as c_int) != 0 {
        return calisto_ruby::Qnil;
    }
    let c = (p.getvalue)(res, i as c_int, j as c_int);
    let len = (p.getlength)(res, i as c_int, j as c_int);
    if unsafe { (*h).decode } {
        match decode_value(vm, (p.ftype)(res, j as c_int), c, len) {
            Some(v) => v,
            None => utf8_string(vm, c, len as c_long),
        }
    } else {
        utf8_string(vm, c, len as c_long)
    }
}

unsafe extern "C" fn res_getisnull(argc: c_int, argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 2 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 2)"));
    }
    let h = res_of(vm, self_);
    let res = check_open_res(vm, h);
    let p = pq();
    let i = (vm.native().rb_num2ll)(*argv);
    let j = (vm.native().rb_num2ll)(*argv.add(1));
    if i < 0 || i >= (p.ntuples)(res) as i64 || j < 0 || j >= (p.nfields)(res) as i64 {
        vm.raise(vm.e_arg_error(), "indice fora do range do result");
    }
    if (p.getisnull)(res, i as c_int, j as c_int) != 0 {
        calisto_ruby::Qtrue
    } else {
        calisto_ruby::Qfalse
    }
}

/// `Result#ftype(i)` — OID da coluna (AR: initializer do type map).
unsafe extern "C" fn res_ftype(argc: c_int, argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 1 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 1)"));
    }
    let h = res_of(vm, self_);
    let res = check_open_res(vm, h);
    let j = (vm.native().rb_num2ll)(*argv);
    if j < 0 || j >= (pq().nfields)(res) as i64 {
        vm.raise(vm.e_arg_error(), "indice fora do range do result");
    }
    (vm.native().rb_ll2inum)((pq().ftype)(res, j as c_int) as i64)
}

/// `Result#fmod(i)` — typmod da coluna.
unsafe extern "C" fn res_fmod(argc: c_int, argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 1 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 1)"));
    }
    let h = res_of(vm, self_);
    let res = check_open_res(vm, h);
    let j = (vm.native().rb_num2ll)(*argv);
    if j < 0 || j >= (pq().nfields)(res) as i64 {
        vm.raise(vm.e_arg_error(), "indice fora do range do result");
    }
    (vm.native().rb_ll2inum)((pq().fmod)(res, j as c_int) as i64)
}

/// `Result#cmd_tuples` — Integer (o pg gem devolve LONG2NUM(strtol(...)),
/// nao String: o AR compara numericamente — `destroy_row > 0`,
/// `affected_rows != 1` do optimistic locking; String "1" > 0 quebra com
/// ArgumentError "comparison of String with 0 failed").
unsafe extern "C" fn res_cmd_tuples(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = res_of(vm, self_);
    let res = check_open_res(vm, h);
    let s = (pq().cmd_tuples)(res);
    let n = if s.is_null() {
        0
    } else {
        let bytes = unsafe { CStr::from_ptr(s) }.to_bytes();
        std::str::from_utf8(bytes)
            .ok()
            .and_then(|t| t.parse::<i64>().ok())
            .unwrap_or(0)
    };
    (vm.native().rb_ll2inum)(n)
}

/// `Result#result_status` — ExecStatusType (int).
unsafe extern "C" fn res_result_status(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = res_of(vm, self_);
    let res = check_open_res(vm, h);
    (vm.native().rb_ll2inum)((pq().result_status)(res) as i64)
}

/// `Result#error_message` — mensagem de erro ("" quando sem erro).
unsafe extern "C" fn res_error_message(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = res_of(vm, self_);
    let res = check_open_res(vm, h);
    vm.str(&result_error_msg(pq(), res))
}

/// `Result#clear` / `#cleared?`
unsafe extern "C" fn res_clear(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = res_of(vm, self_);
    let res = unsafe { (*h).res };
    if !res.is_null() {
        (pq().clear)(res);
        unsafe { (*h).res = std::ptr::null_mut() };
    }
    calisto_ruby::Qnil
}

unsafe extern "C" fn res_cleared(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = res_of(vm, self_);
    if unsafe { (*h).res.is_null() } {
        calisto_ruby::Qtrue
    } else {
        calisto_ruby::Qfalse
    }
}

/// `Result#map_types!(_type_map)` — no-op devolvendo self (a conversao
/// nativa devolve strings; o typecast do AR e a fonte da verdade).
unsafe extern "C" fn res_map_types(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 1 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 1)"));
    }
    self_
}

// ---- registro ------------------------------------------------------------------

fn define_method(vm: &Ruby, klass: VALUE, name: &'static str, f: unsafe extern "C" fn(c_int, *mut VALUE, VALUE) -> VALUE) {
    let cname = CString::new(name).unwrap();
    unsafe {
        (vm.native().rb_define_method)(klass, cname.as_ptr(), f as *const c_void, -1);
    }
}

fn define_singleton(vm: &Ruby, obj: VALUE, name: &'static str, f: unsafe extern "C" fn(c_int, *mut VALUE, VALUE) -> VALUE) {
    let cname = CString::new(name).unwrap();
    unsafe {
        (vm.native().rb_define_singleton_method)(obj, cname.as_ptr(), f as *const c_void, -1);
    }
}

/// `PG.connect(...)` — delega para `PG::Connection.new` (AR: `PG.connect(**params)`).
unsafe extern "C" fn pg_connect(argc: c_int, argv: *mut VALUE, _self: VALUE) -> VALUE {
    let vm = vm();
    let cls = connection_class();
    match argc {
        0 => vm.funcall(cls, "new", &[]),
        1 => vm.funcall(cls, "new", &[*argv]),
        _ => vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0..1)")),
    }
}

/// `PG.library_version` — int (AR checa libpq >= 18 para o cancel).
unsafe extern "C" fn pg_library_version(argc: c_int, _argv: *mut VALUE, _self: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    (vm.native().rb_ll2inum)((pq().lib_version)() as i64)
}

/// Registra o modulo `PG` na VM. Chamado no boot do daemon (best-effort:
/// Err quando nenhuma libpq e encontravel — o daemon avisa e segue; o shim
/// do require levanta LoadError claro).
pub fn register(vm: &Ruby) -> Result<(), String> {
    let fns = Box::new(pq_open_lib()?);
    *VM.lock().unwrap() = Some(vm as *const Ruby as usize);
    *PQ.lock().unwrap() = Some(fns);
    let n = vm.native();
    unsafe {
        let pg = (n.rb_define_module)(c"PG".as_ptr());
        // marcador do shim gems/pg.rb: distingue o pg nativo da gem real
        // (fallback sem libpq) para nao sobrescrever as classes dela
        vm.funcall(pg, "const_set", &[vm.str("CALISTO_NATIVE"), calisto_ruby::Qtrue]);
        let error = (n.rb_define_class_under)(pg, c"Error".as_ptr(), vm.e_standard_error());
        let bad = (n.rb_define_class_under)(pg, c"ConnectionBad".as_ptr(), error);
        let unsupported = (n.rb_define_class_under)(pg, c"FeatureNotSupported".as_ptr(), error);
        let conn = (n.rb_define_class_under)(pg, c"Connection".as_ptr(), vm.c_object());
        let res = (n.rb_define_class_under)(pg, c"Result".as_ptr(), vm.c_object());
        (n.rb_define_alloc_func)(conn, conn_alloc);
        (n.rb_define_alloc_func)(res, res_alloc);
        *CLASS_ERROR.lock().unwrap() = Some(error);
        *CLASS_CONNECTION_BAD.lock().unwrap() = Some(bad);
        *CLASS_CONNECTION.lock().unwrap() = Some(conn);
        *CLASS_RESULT.lock().unwrap() = Some(res);
        let _ = unsupported;
        define_singleton(vm, pg, "connect", pg_connect);
        define_singleton(vm, pg, "library_version", pg_library_version);
        define_singleton(vm, conn, "conndefaults_hash", conn_conndefaults_hash);
        define_singleton(vm, conn, "quote_ident", conn_quote_ident);
        define_method(vm, conn, "initialize", conn_initialize);
        define_method(vm, conn, "exec", conn_exec);
        define_method(vm, conn, "query", conn_exec);
        define_method(vm, conn, "exec_params", conn_exec_params);
        define_method(vm, conn, "async_exec", conn_exec);
        define_method(vm, conn, "async_exec_params", conn_exec_params);
        define_method(vm, conn, "prepare", conn_prepare);
        define_method(vm, conn, "exec_prepared", conn_exec_prepared);
        define_method(vm, conn, "async_prepare", conn_prepare);
        define_method(vm, conn, "async_exec_prepared", conn_exec_prepared);
        define_method(vm, conn, "escape", conn_escape);
        define_method(vm, conn, "escape_literal", conn_escape_literal);
        define_method(vm, conn, "escape_identifier", conn_escape_identifier);
        define_method(vm, conn, "parameter_status", conn_parameter_status);
        define_method(vm, conn, "server_version", conn_server_version);
        define_method(vm, conn, "transaction_status", conn_transaction_status);
        define_method(vm, conn, "status", conn_status);
        define_method(vm, conn, "db", conn_db);
        define_method(vm, conn, "user", conn_user);
        define_method(vm, conn, "host", conn_host);
        define_method(vm, conn, "port", conn_port);
        define_method(vm, conn, "close", conn_close);
        define_method(vm, conn, "finish", conn_close);
        define_method(vm, conn, "closed?", conn_closed);
        define_method(vm, conn, "finished?", conn_closed);
        define_method(vm, conn, "reset", conn_reset);
        define_method(vm, conn, "cancel", conn_cancel);
        define_method(vm, conn, "block", conn_block);
        define_method(vm, conn, "socket_io", conn_socket_io);
        define_method(vm, conn, "set_client_encoding", conn_set_client_encoding);
        define_method(vm, conn, "set_notice_receiver", conn_set_notice_receiver);
        define_method(vm, conn, "_calisto_result_type_map!", conn_calisto_result_type_map);
        define_method(vm, conn, "get_last_result", conn_get_last_result);
        define_method(vm, res, "values", res_values);
        define_method(vm, res, "rows", res_values);
        define_method(vm, res, "fields", res_fields);
        define_method(vm, res, "ntuples", res_ntuples);
        define_method(vm, res, "nfields", res_nfields);
        define_method(vm, res, "[]", res_get);
        define_method(vm, res, "getvalue", res_getvalue);
        define_method(vm, res, "typed_getvalue", res_typed_getvalue);
        define_method(vm, res, "getisnull", res_getisnull);
        define_method(vm, res, "ftype", res_ftype);
        define_method(vm, res, "fmod", res_fmod);
        define_method(vm, res, "cmd_tuples", res_cmd_tuples);
        define_method(vm, res, "result_status", res_result_status);
        define_method(vm, res, "error_message", res_error_message);
        define_method(vm, res, "clear", res_clear);
        define_method(vm, res, "cleared?", res_cleared);
        define_method(vm, res, "map_types!", res_map_types);
    }
    Ok(())
}
