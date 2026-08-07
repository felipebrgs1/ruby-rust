//! calisto-sqlite — binding nativo `Calisto::SQLite` (Fase P do roadmap).
//!
//! FFI hand-rolled (zero crates, convencao do repo) sobre a `libsqlite3.so`
//! do sistema (dlopen, como a libruby do calisto-ruby). O modulo Ruby e
//! registrado no boot do daemon (rb_define_method via calisto-ruby) e os
//! children do fork herdam os metodos.
//!
//! API (forma do `bun:sqlite`, escopo minimo do roadmap):
//!   Calisto::SQLite.open(path | :memory:) -> Database
//!   Database#execute(sql, *binds)   -> Array de linhas ([] p/ nao-SELECT)
//!   Database#prepare(sql)           -> Statement (reutilizavel)
//!   Database#changes / #last_insert_rowid / #close / #closed?
//!   Statement#execute(*binds)       -> reset + re-bind + executa
//!   Statement#columns / #close / #closed?
//!   Calisto::SQLite::Error < StandardError
//!
//! Handles via TypedData do CRuby (rb_data_typed_object_wrap) — o dfree
//! fecha o handle no GC. Erros viram `Calisto::SQLite::Error` via
//! rb_exc_raise (longjmp protegido pelo dispatch da VM), com o sqlite3_errmsg.

use calisto_ruby::{RbDataType, RbDataTypeFunction, Ruby, VALUE};
use std::ffi::{c_char, c_int, c_long, c_void, CStr, CString};
use std::sync::Mutex;

// ---- sqlite3 (C API estavel desde sempre) -----------------------------------

#[repr(C)]
struct sqlite3 {
    _private: [u8; 0],
}
#[repr(C)]
struct sqlite3_stmt {
    _private: [u8; 0],
}

const SQLITE_OK: c_int = 0;
const SQLITE_ROW: c_int = 100;
const SQLITE_DONE: c_int = 101;
const SQLITE_OPEN_READWRITE: c_int = 2;
const SQLITE_OPEN_CREATE: c_int = 4;
const SQLITE_TRANSIENT: *mut c_void = usize::MAX as *mut c_void;

const SQLITE_INTEGER: c_int = 1;
const SQLITE_FLOAT: c_int = 2;
const SQLITE_TEXT: c_int = 3;
const SQLITE_BLOB: c_int = 4;

struct SqliteFns {
    open_v2: unsafe extern "C" fn(*const c_char, *mut *mut sqlite3, c_int, *const c_char) -> c_int,
    close_v2: unsafe extern "C" fn(*mut sqlite3) -> c_int,
    prepare_v2: unsafe extern "C" fn(*mut sqlite3, *const c_char, c_int, *mut *mut sqlite3_stmt, *mut *const c_char) -> c_int,
    finalize: unsafe extern "C" fn(*mut sqlite3_stmt) -> c_int,
    step: unsafe extern "C" fn(*mut sqlite3_stmt) -> c_int,
    reset: unsafe extern "C" fn(*mut sqlite3_stmt) -> c_int,
    clear_bindings: unsafe extern "C" fn(*mut sqlite3_stmt) -> c_int,
    bind_null: unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> c_int,
    bind_int64: unsafe extern "C" fn(*mut sqlite3_stmt, c_int, i64) -> c_int,
    bind_double: unsafe extern "C" fn(*mut sqlite3_stmt, c_int, f64) -> c_int,
    bind_text: unsafe extern "C" fn(*mut sqlite3_stmt, c_int, *const c_char, c_int, *mut c_void) -> c_int,
    column_count: unsafe extern "C" fn(*mut sqlite3_stmt) -> c_int,
    column_name: unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> *const c_char,
    column_type: unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> c_int,
    column_int64: unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> i64,
    column_double: unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> f64,
    column_text: unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> *const c_void,
    column_blob: unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> *const c_void,
    column_bytes: unsafe extern "C" fn(*mut sqlite3_stmt, c_int) -> c_int,
    errmsg: unsafe extern "C" fn(*mut sqlite3) -> *const c_char,
    errstr: unsafe extern "C" fn(c_int) -> *const c_char,
    changes: unsafe extern "C" fn(*mut sqlite3) -> c_int,
    last_insert_rowid: unsafe extern "C" fn(*mut sqlite3) -> i64,
    libversion: unsafe extern "C" fn() -> *const c_char,
}

fn last_dlerror() -> String {
    let p = unsafe { std::ffi::CStr::from_ptr(dlerror()) };
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
            "simbolo '{}' nao encontrado na libsqlite3: {}",
            String::from_utf8_lossy(&name[..name.len() - 1]),
            last_dlerror()
        ));
    }
    Ok(std::mem::transmute_copy(&sym))
}

/// dlopen da libsqlite3 do sistema (SONAME .so.0, fallback .so). Err claro
/// se a lib nao existir — o daemon degrada (avisa e segue; o require do
/// shim levanta LoadError).
fn sqlite_open_lib() -> Result<SqliteFns, String> {
    let mut handle: *mut c_void = std::ptr::null_mut();
    for name in [&b"libsqlite3.so.0\0"[..], &b"libsqlite3.so\0"[..]] {
        let c = CString::new(&name[..name.len() - 1]).unwrap();
        handle = unsafe { dlopen(c.as_ptr(), 2) }; // RTLD_NOW
        if !handle.is_null() {
            break;
        }
    }
    if handle.is_null() {
        return Err(format!("dlopen libsqlite3: {}", last_dlerror()));
    }
    unsafe {
        Ok(SqliteFns {
            open_v2: load_sym(handle, b"sqlite3_open_v2\0")?,
            close_v2: load_sym(handle, b"sqlite3_close_v2\0")?,
            prepare_v2: load_sym(handle, b"sqlite3_prepare_v2\0")?,
            finalize: load_sym(handle, b"sqlite3_finalize\0")?,
            step: load_sym(handle, b"sqlite3_step\0")?,
            reset: load_sym(handle, b"sqlite3_reset\0")?,
            clear_bindings: load_sym(handle, b"sqlite3_clear_bindings\0")?,
            bind_null: load_sym(handle, b"sqlite3_bind_null\0")?,
            bind_int64: load_sym(handle, b"sqlite3_bind_int64\0")?,
            bind_double: load_sym(handle, b"sqlite3_bind_double\0")?,
            bind_text: load_sym(handle, b"sqlite3_bind_text\0")?,
            column_count: load_sym(handle, b"sqlite3_column_count\0")?,
            column_name: load_sym(handle, b"sqlite3_column_name\0")?,
            column_type: load_sym(handle, b"sqlite3_column_type\0")?,
            column_int64: load_sym(handle, b"sqlite3_column_int64\0")?,
            column_double: load_sym(handle, b"sqlite3_column_double\0")?,
            column_text: load_sym(handle, b"sqlite3_column_text\0")?,
            column_blob: load_sym(handle, b"sqlite3_column_blob\0")?,
            column_bytes: load_sym(handle, b"sqlite3_column_bytes\0")?,
            errmsg: load_sym(handle, b"sqlite3_errmsg\0")?,
            errstr: load_sym(handle, b"sqlite3_errstr\0")?,
            changes: load_sym(handle, b"sqlite3_changes\0")?,
            last_insert_rowid: load_sym(handle, b"sqlite3_last_insert_rowid\0")?,
            libversion: load_sym(handle, b"sqlite3_libversion\0")?,
        })
    }
}

// ---- VM + classes registradas -------------------------------------------------

static VM: Mutex<Option<usize>> = Mutex::new(None);
/// SqliteFns num Box (heap) — o ponteiro do stack do register() morreria.
static SQLITE: Mutex<Option<Box<SqliteFns>>> = Mutex::new(None);
static CLASS_ERROR: Mutex<Option<VALUE>> = Mutex::new(None);
static CLASS_DATABASE: Mutex<Option<VALUE>> = Mutex::new(None);
static CLASS_STATEMENT: Mutex<Option<VALUE>> = Mutex::new(None);

fn vm() -> &'static Ruby {
    let g = VM.lock().unwrap();
    unsafe { &*(g.expect("calisto-sqlite: register() nao chamado") as *const Ruby) }
}

fn sqlite() -> &'static SqliteFns {
    let g = SQLITE.lock().unwrap();
    let b = g.as_ref().expect("calisto-sqlite: register() nao chamado");
    let p: *const SqliteFns = &**b;
    unsafe { &*p }
}

fn error_class() -> VALUE {
    CLASS_ERROR.lock().unwrap().expect("error class")
}

// ---- TypedData (handles db/stmt com dfree no GC) ------------------------------

/// Ponteiro cru num static exige Sync — o conteudo nunca e mutado depois da
/// inicializacao const (wrap_struct_name/functions/parent sao estaticos), o
/// unsafe impl e sound.
struct SyncPtr<T>(T);
unsafe impl<T> Sync for SyncPtr<T> {}

unsafe extern "C" fn db_free(p: *mut c_void) {
    if !p.is_null() {
        let s = sqlite();
        unsafe { (s.close_v2)(p as *mut sqlite3) };
    }
}

unsafe extern "C" fn stmt_free(p: *mut c_void) {
    if !p.is_null() {
        let s = sqlite();
        unsafe { (s.finalize)(p as *mut sqlite3_stmt) };
    }
}

/// Alloc do padrão de C extension: instancia vazia do TypedData (data NULL)
/// — `Database.new`/`Statement.new` existem mas ficam num estado que os
/// metodos rejeitam (rb_check_typeddata com data nulo -> raise). Sem o
/// alloc proprio, o CRuby avisa "undefining the allocator of T_DATA class"
/// a cada instancia (rb_data_object_check do gc.c).
unsafe extern "C" fn db_alloc(klass: VALUE) -> VALUE {
    let n = vm().native();
    (n.rb_data_typed_object_wrap)(klass, std::ptr::null_mut(), db_type())
}

unsafe extern "C" fn stmt_alloc(klass: VALUE) -> VALUE {
    let n = vm().native();
    (n.rb_data_typed_object_wrap)(klass, std::ptr::null_mut(), stmt_type())
}

static DB_TYPE: SyncPtr<RbDataType> = SyncPtr(RbDataType {
    wrap_struct_name: c"Calisto::SQLite::Database".as_ptr(),
    function: RbDataTypeFunction {
        dmark: None,
        dfree: Some(db_free),
        dsize: None,
        dcompact: None,
        reserved: [std::ptr::null_mut()],
    },
    parent: std::ptr::null(),
    data: std::ptr::null_mut(),
    flags: calisto_ruby::RUBY_TYPED_FREE_IMMEDIATELY,
});

static STMT_TYPE: SyncPtr<RbDataType> = SyncPtr(RbDataType {
    wrap_struct_name: c"Calisto::SQLite::Statement".as_ptr(),
    function: RbDataTypeFunction {
        dmark: None,
        dfree: Some(stmt_free),
        dsize: None,
        dcompact: None,
        reserved: [std::ptr::null_mut()],
    },
    parent: std::ptr::null(),
    data: std::ptr::null_mut(),
    flags: calisto_ruby::RUBY_TYPED_FREE_IMMEDIATELY,
});

fn db_type() -> &'static RbDataType {
    &DB_TYPE.0
}

fn stmt_type() -> &'static RbDataType {
    &STMT_TYPE.0
}

/// Handle de Database: `db` nulo = fechado. O struct fica no heap do
/// TypedData (o dfree fecha o sqlite3 no GC).
#[repr(C)]
struct Db {
    db: *mut sqlite3,
}

/// Handle de Statement: guarda o db para o errmsg; `stmt` nulo = fechado.
#[repr(C)]
struct Stmt {
    stmt: *mut sqlite3_stmt,
    db: *mut sqlite3,
}

fn db_of(vm: &Ruby, obj: VALUE) -> *mut Db {
    let p = unsafe { (vm.native().rb_check_typeddata)(obj, db_type()) };
    if p.is_null() {
        vm.raise(error_class(), "objeto invalido (Calisto::SQLite::Database)");
    }
    p as *mut Db
}

fn stmt_of(vm: &Ruby, obj: VALUE) -> *mut Stmt {
    let p = unsafe { (vm.native().rb_check_typeddata)(obj, stmt_type()) };
    if p.is_null() {
        vm.raise(error_class(), "objeto invalido (Calisto::SQLite::Statement)");
    }
    p as *mut Stmt
}

fn err_raise(vm: &Ruby, db: *mut sqlite3, rc: c_int, what: &str) -> ! {
    let s = sqlite();
    let msg = if db.is_null() {
        unsafe { CStr::from_ptr((s.errstr)(rc)) }.to_string_lossy().into_owned()
    } else {
        let p = unsafe { (s.errmsg)(db) };
        if p.is_null() { format!("{what} (erro {rc})") } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
    };
    vm.raise(error_class(), &format!("{what}: {msg}"))
}

// ---- metodos nativos -----------------------------------------------------------

/// `Calisto::SQLite.open(path | :memory:)` -> Database
unsafe extern "C" fn sqlite_open(argc: c_int, argv: *mut VALUE, _self: VALUE) -> VALUE {
    let vm = vm();
    if argc != 1 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 1)"));
    }
    let v = *argv;
    let is_memory = {
        let n = vm.native();
        let sym = (n.rb_id2sym)(vm.intern("memory"));
        v == sym
    };
    let path_c = if is_memory {
        CString::new(":memory:").unwrap()
    } else {
        let mut sv = v;
        let (ptr, len) = vm.string_bytes(&mut sv);
        let slice = std::slice::from_raw_parts(ptr, len);
        match CString::new(slice) {
            Ok(c) => c,
            Err(_) => vm.raise(vm.e_arg_error(), "path de banco nao pode conter NUL"),
        }
    };
    let s = sqlite();
    let mut db: *mut sqlite3 = std::ptr::null_mut();
    let rc = (s.open_v2)(path_c.as_ptr(), &mut db, SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE, std::ptr::null());
    if rc != SQLITE_OK {
        err_raise(vm, db, rc, "cannot open database");
    }
    let boxed = Box::into_raw(Box::new(Db { db }));
    (vm.native().rb_data_typed_object_wrap)(CLASS_DATABASE.lock().unwrap().unwrap(), boxed as *mut c_void, db_type())
}

/// `Calisto::SQLite.libversion` -> String
unsafe extern "C" fn sqlite_libversion(argc: c_int, _argv: *mut VALUE, _self: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let s = sqlite();
    let p = (s.libversion)();
    let v = CStr::from_ptr(p).to_string_lossy().into_owned();
    vm.str(&v)
}

fn check_open_db(vm: &Ruby, h: *mut Db) -> *mut sqlite3 {
    let db = unsafe { (*h).db };
    if db.is_null() {
        vm.raise(error_class(), "database is closed");
    }
    db
}

fn check_open_stmt(vm: &Ruby, h: *mut Stmt) -> *mut sqlite3_stmt {
    let stmt = unsafe { (*h).stmt };
    if stmt.is_null() {
        vm.raise(error_class(), "statement is closed");
    }
    stmt
}

/// Converte um VALUE para bind param (nil/bool/Integer/Float/String);
/// TypeError para outros tipos. `idx` e 1-based (API do sqlite).
fn bind_param(vm: &Ruby, stmt: *mut sqlite3_stmt, db: *mut sqlite3, idx: c_int, v: VALUE) {
    let s = sqlite();
    let n = vm.native();
    let rc = unsafe {
        if v == calisto_ruby::Qnil {
            (s.bind_null)(stmt, idx)
        } else if v == calisto_ruby::Qtrue || v == calisto_ruby::Qfalse {
            (s.bind_int64)(stmt, idx, if v == calisto_ruby::Qtrue { 1 } else { 0 })
        } else if (v & 1) == 1 {
            // Fixnum (INT2FIX: valor deslocado 1 bit)
            (s.bind_int64)(stmt, idx, (v as i64) >> 1)
        } else if vm.is_kind_of(v, vm.c_string()) {
            let mut sv = v;
            let (ptr, len) = vm.string_bytes(&mut sv);
            (s.bind_text)(stmt, idx, ptr as *const c_char, len as c_int, SQLITE_TRANSIENT)
        } else if vm.is_kind_of(v, vm.c_integer()) {
            let i = (n.rb_num2ll)(v); // RangeError p/ bignum enorme
            (s.bind_int64)(stmt, idx, i)
        } else if vm.is_kind_of(v, vm.c_float()) {
            (s.bind_double)(stmt, idx, (n.rb_num2dbl)(v))
        } else {
            vm.raise(vm.e_type_error(), &format!("cannot bind {} as SQLite param", vm.classname(v)));
        }
    };
    if rc != SQLITE_OK {
        err_raise(vm, db, rc, "bind");
    }
}

/// Executa a statement ate DONE, coletando linhas; [] para nao-SELECT.
/// `stmt` e finalizada no fim (execute de Database). Err via raise.
fn run_statement(vm: &Ruby, db: *mut sqlite3, stmt: *mut sqlite3_stmt) -> VALUE {
    let s = sqlite();
    let ncol = unsafe { (s.column_count)(stmt) };
    let rows = unsafe { (vm.native().rb_ary_new)() };
    let n = vm.native();
    loop {
        let rc = unsafe { (s.step)(stmt) };
        if rc == SQLITE_ROW {
            if ncol > 0 {
                let row = unsafe { (n.rb_ary_new)() };
                for c in 0..ncol {
                    let val = unsafe {
                        match (s.column_type)(stmt, c) {
                            SQLITE_INTEGER => (n.rb_ll2inum)((s.column_int64)(stmt, c)),
                            SQLITE_FLOAT => (n.rb_float_new)((s.column_double)(stmt, c)),
                            SQLITE_TEXT => {
                                let p = (s.column_text)(stmt, c) as *const u8;
                                let len = (s.column_bytes)(stmt, c);
                                if p.is_null() {
                                    calisto_ruby::Qnil
                                } else {
                                    (n.rb_utf8_str_new)(p as *const c_char, len as c_long)
                                }
                            }
                            SQLITE_BLOB => {
                                let p = (s.column_blob)(stmt, c) as *const u8;
                                let len = (s.column_bytes)(stmt, c);
                                if p.is_null() {
                                    calisto_ruby::Qnil
                                } else {
                                    (n.rb_str_new)(p as *const c_char, len as c_long)
                                }
                            }
                            _ => calisto_ruby::Qnil, // SQLITE_NULL
                        }
                    };
                    unsafe { (n.rb_ary_push)(row, val) };
                }
                unsafe { (n.rb_ary_push)(rows, row) };
            }
        } else if rc == SQLITE_DONE {
            break;
        } else {
            err_raise(vm, db, rc, "step");
        }
    }
    rows
}

/// `Database#execute(sql, *binds)` -> Array
unsafe extern "C" fn db_execute(argc: c_int, argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    let h = db_of(vm, self_);
    let db = check_open_db(vm, h);
    if argc < 1 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 1+)"));
    }
    let mut sv = *argv;
    let (ptr, len) = vm.string_bytes(&mut sv);
    let sql_c = match CString::new(std::slice::from_raw_parts(ptr, len)) {
        Ok(c) => c,
        Err(_) => vm.raise(vm.e_arg_error(), "SQL nao pode conter NUL"),
    };
    let s = sqlite();
    let mut stmt: *mut sqlite3_stmt = std::ptr::null_mut();
    let mut tail: *const c_char = std::ptr::null();
    let rc = (s.prepare_v2)(db, sql_c.as_ptr(), -1, &mut stmt, &mut tail);
    if rc != SQLITE_OK {
        err_raise(vm, db, rc, "prepare");
    }
    // multi-statement: tail nao-vazio alem de espacos/';'
    let tail_nonempty = if tail.is_null() {
        false
    } else {
        let t = unsafe { CStr::from_ptr(tail) }.to_string_lossy();
        !t.trim().trim_end_matches(';').trim().is_empty()
    };
    if tail_nonempty {
        unsafe { (s.finalize)(stmt) };
        vm.raise(error_class(), "multiple SQL statements nao suportado (uma por chamada)");
    }
    for i in 0..(argc - 1) {
        let v = *argv.add(1 + i as usize);
        bind_param(vm, stmt, db, i + 1, v);
    }
    let rows = run_statement(vm, db, stmt);
    unsafe { (s.finalize)(stmt) };
    rows
}

/// `Database#prepare(sql)` -> Statement
unsafe extern "C" fn db_prepare(argc: c_int, argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    let h = db_of(vm, self_);
    let db = check_open_db(vm, h);
    if argc != 1 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 1)"));
    }
    let mut sv = *argv;
    let (ptr, len) = vm.string_bytes(&mut sv);
    let sql_c = match CString::new(std::slice::from_raw_parts(ptr, len)) {
        Ok(c) => c,
        Err(_) => vm.raise(vm.e_arg_error(), "SQL nao pode conter NUL"),
    };
    let s = sqlite();
    let mut stmt: *mut sqlite3_stmt = std::ptr::null_mut();
    let mut tail: *const c_char = std::ptr::null();
    let rc = (s.prepare_v2)(db, sql_c.as_ptr(), -1, &mut stmt, &mut tail);
    if rc != SQLITE_OK {
        err_raise(vm, db, rc, "prepare");
    }
    let tail_nonempty = if tail.is_null() {
        false
    } else {
        let t = unsafe { CStr::from_ptr(tail) }.to_string_lossy();
        !t.trim().trim_end_matches(';').trim().is_empty()
    };
    if tail_nonempty {
        unsafe { (s.finalize)(stmt) };
        vm.raise(error_class(), "multiple SQL statements nao suportado (uma por chamada)");
    }
    let boxed = Box::into_raw(Box::new(Stmt { stmt, db }));
    (vm.native().rb_data_typed_object_wrap)(CLASS_STATEMENT.lock().unwrap().unwrap(), boxed as *mut c_void, stmt_type())
}

/// `Statement#execute(*binds)` -> Array (reset + re-bind a cada chamada)
unsafe extern "C" fn stmt_execute(argc: c_int, argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    let h = stmt_of(vm, self_);
    let stmt = check_open_stmt(vm, h);
    let db = unsafe { (*h).db };
    let s = sqlite();
    let rc = (s.reset)(stmt);
    if rc != SQLITE_OK {
        err_raise(vm, db, rc, "reset");
    }
    let rc = (s.clear_bindings)(stmt);
    if rc != SQLITE_OK {
        err_raise(vm, db, rc, "clear_bindings");
    }
    for i in 0..argc {
        let v = *argv.add(i as usize);
        bind_param(vm, stmt, db, i + 1, v);
    }
    run_statement(vm, db, stmt)
}

/// `Statement#columns` -> Array<String>
unsafe extern "C" fn stmt_columns(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = stmt_of(vm, self_);
    let stmt = check_open_stmt(vm, h);
    let s = sqlite();
    let n = vm.native();
    let ncol = (s.column_count)(stmt);
    let cols = unsafe { (n.rb_ary_new)() };
    for c in 0..ncol {
        let name = (s.column_name)(stmt, c);
        let sv = if name.is_null() {
            vm.str("")
        } else {
            vm.str(&unsafe { CStr::from_ptr(name) }.to_string_lossy())
        };
        unsafe { (n.rb_ary_push)(cols, sv) };
    }
    cols
}

/// `Database#changes` / `Database#last_insert_rowid` / `#close` / `#closed?`
unsafe extern "C" fn db_changes(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = db_of(vm, self_);
    let db = check_open_db(vm, h);
    let s = sqlite();
    (vm.native().rb_ll2inum)((s.changes)(db) as i64)
}

unsafe extern "C" fn db_last_insert_rowid(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = db_of(vm, self_);
    let db = check_open_db(vm, h);
    let s = sqlite();
    (vm.native().rb_ll2inum)((s.last_insert_rowid)(db))
}

unsafe extern "C" fn db_close(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = db_of(vm, self_);
    let db = check_open_db(vm, h);
    let s = sqlite();
    let rc = (s.close_v2)(db);
    if rc != SQLITE_OK {
        err_raise(vm, db, rc, "close");
    }
    unsafe { (*h).db = std::ptr::null_mut() };
    calisto_ruby::Qnil
}

unsafe extern "C" fn db_closed(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = db_of(vm, self_);
    if unsafe { (*h).db.is_null() } { calisto_ruby::Qtrue } else { calisto_ruby::Qfalse }
}

unsafe extern "C" fn stmt_close(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = stmt_of(vm, self_);
    let stmt = check_open_stmt(vm, h);
    let s = sqlite();
    let rc = (s.finalize)(stmt);
    if rc != SQLITE_OK {
        err_raise(vm, unsafe { (*h).db }, rc, "close");
    }
    unsafe { (*h).stmt = std::ptr::null_mut() };
    calisto_ruby::Qnil
}

unsafe extern "C" fn stmt_closed(argc: c_int, _argv: *mut VALUE, self_: VALUE) -> VALUE {
    let vm = vm();
    if argc != 0 {
        vm.raise(vm.e_arg_error(), &format!("wrong number of arguments (given {argc}, expected 0)"));
    }
    let h = stmt_of(vm, self_);
    if unsafe { (*h).stmt.is_null() } { calisto_ruby::Qtrue } else { calisto_ruby::Qfalse }
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

/// Registra o modulo `Calisto::SQLite` na VM. Chamado no boot do daemon
/// (best-effort: Err quando a libsqlite3 do sistema nao existe — o daemon
/// avisa e segue; o shim do require levanta LoadError).
pub fn register(vm: &Ruby) -> Result<(), String> {
    let fns = Box::new(sqlite_open_lib()?);
    *VM.lock().unwrap() = Some(vm as *const Ruby as usize);
    *SQLITE.lock().unwrap() = Some(fns);
    let n = vm.native();
    unsafe {
        let calisto = (n.rb_define_module)(c"Calisto".as_ptr());
        let sqlite_mod = (n.rb_define_module_under)(calisto, c"SQLite".as_ptr());
        let error = (n.rb_define_class_under)(sqlite_mod, c"Error".as_ptr(), vm.e_standard_error());
        let database = (n.rb_define_class_under)(sqlite_mod, c"Database".as_ptr(), vm.c_object());
        let statement = (n.rb_define_class_under)(sqlite_mod, c"Statement".as_ptr(), vm.c_object());
        (n.rb_define_alloc_func)(database, db_alloc);
        (n.rb_define_alloc_func)(statement, stmt_alloc);
        *CLASS_ERROR.lock().unwrap() = Some(error);
        *CLASS_DATABASE.lock().unwrap() = Some(database);
        *CLASS_STATEMENT.lock().unwrap() = Some(statement);
        define_singleton(vm, sqlite_mod, "open", sqlite_open);
        define_singleton(vm, sqlite_mod, "libversion", sqlite_libversion);
        define_method(vm, database, "execute", db_execute);
        define_method(vm, database, "prepare", db_prepare);
        define_method(vm, database, "changes", db_changes);
        define_method(vm, database, "last_insert_rowid", db_last_insert_rowid);
        define_method(vm, database, "close", db_close);
        define_method(vm, database, "closed?", db_closed);
        define_method(vm, statement, "execute", stmt_execute);
        define_method(vm, statement, "columns", stmt_columns);
        define_method(vm, statement, "close", stmt_close);
        define_method(vm, statement, "closed?", stmt_closed);
    }
    Ok(())
}
