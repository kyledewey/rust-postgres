use std::libc::c_int;
use std::ptr;
use std::str;

mod ffi {
    use std::libc::{c_char, c_int, c_void};
    use std::cast;

    pub type sqlite3 = c_void;
    pub type sqlite3_stmt = c_void;

    pub static SQLITE_OK: c_int = 0;
    pub static SQLITE_ROW: c_int = 100;
    pub static SQLITE_DONE: c_int = 101;

    // A function because Rust doesn't like casting from an int to a function
    // pointer in a static declaration
    pub fn SQLITE_TRANSIENT() -> extern "C" fn(*c_void) {
        unsafe { cast::transmute(-1) }
    }

    #[link_args = "-lsqlite3"]
    extern "C" {
        fn sqlite3_open(filename: *c_char, ppDb: *mut *sqlite3) -> c_int;
        fn sqlite3_close(db: *sqlite3) -> c_int;
        fn sqlite3_errmsg(db: *sqlite3) -> *c_char;
        fn sqlite3_changes(db: *sqlite3) -> c_int;
        fn sqlite3_prepare_v2(db: *sqlite3, zSql: *c_char, nByte: c_int,
                              ppStmt: *mut *sqlite3_stmt, pzTail: *mut *c_char)
                              -> c_int;
        fn sqlite3_reset(pStmt: *sqlite3_stmt) -> c_int;
        fn sqlite3_bind_text(pStmt: *sqlite3_stmt, idx: c_int, text: *c_char,
                             n: c_int, free: extern "C" fn(*c_void)) -> c_int;
        fn sqlite3_step(pStmt: *sqlite3_stmt) -> c_int;
        fn sqlite3_column_count(pStmt: *sqlite3_stmt) -> c_int;
        fn sqlite3_column_text(pStmt: *sqlite3_stmt, iCol: c_int) -> *c_char;
        fn sqlite3_finalize(pStmt: *sqlite3_stmt) -> c_int;
    }
}

pub fn open(filename: &str) -> Result<~Connection, ~str> {
    let mut conn = ~Connection {conn: ptr::null()};
    let ret = do filename.as_c_str |c_filename| {
        unsafe { ffi::sqlite3_open(c_filename, &mut conn.conn) }
    };

    match ret {
        ffi::SQLITE_OK => Ok(conn),
        _ => Err(conn.get_error())
    }
}

pub struct Connection {
    priv conn: *ffi::sqlite3
}

impl Drop for Connection {
    fn drop(&self) {
        let ret = unsafe { ffi::sqlite3_close(self.conn) };
        assert!(ret == ffi::SQLITE_OK);
    }
}

impl Connection {
    fn get_error(&self) -> ~str {
        unsafe {
            str::raw::from_c_str(ffi::sqlite3_errmsg(self.conn))
        }
    }
}

macro_rules! ret_err(
    ($mat:expr { $($p:pat => $blk:expr),+ }) => (
        match $mat {
            Err(err) => return Err(err),
            $(
                $p => $blk,
            )+
        }
    );

    ($mat:expr) => (
        match $mat {
            Err(err) => return Err(err),
            _ => ()
        }
    )
)

impl Connection {
    pub fn prepare<'a>(&'a self, query: &str)
                       -> Result<~PreparedStatement<'a>, ~str> {
        let mut stmt = ~PreparedStatement {conn: self, stmt: ptr::null()};
        let ret = do query.as_c_str |c_query| {
            unsafe {
                ffi::sqlite3_prepare_v2(self.conn, c_query, -1, &mut stmt.stmt,
                                        ptr::mut_null())
            }
        };

        match ret {
            ffi::SQLITE_OK => Ok(stmt),
            _ => Err(self.get_error())
        }
    }

    pub fn update(&self, query: &str) -> Result<uint, ~str> {
        self.update_params(query, [])
    }

    pub fn update_params(&self, query: &str, params: &[@SqlType])
                         -> Result<uint, ~str> {
        self.prepare(query).chain(|stmt| stmt.update_params(params))
    }

    pub fn query<T>(&self, query: &str, blk: &fn (&mut ResultIterator) -> T)
                    -> Result<T, ~str> {
        self.query_params(query, [], blk)
    }

    pub fn query_params<T>(&self, query: &str, params: &[@SqlType],
                           blk: &fn (&mut ResultIterator) -> T)
                           -> Result<T, ~str> {
        let stmt = ret_err!(self.prepare(query) { Ok(stmt) => stmt });
        let mut it = ret_err!(stmt.query_params(params) { Ok(it) => it });
        Ok(blk(&mut it))
    }

    pub fn in_transaction<T>(&self, blk: &fn(&Connection) -> Result<T, ~str>)
                             -> Result<T, ~str> {
        ret_err!(self.update("BEGIN"));

        let ret = blk(self);

        // TODO: What to do with errors here?
        match ret {
            Ok(_) => self.update("COMMIT"),
            Err(_) => self.update("ROLLBACK")
        };

        ret
    }
}

pub struct PreparedStatement<'self> {
    priv conn: &'self Connection,
    priv stmt: *ffi::sqlite3_stmt
}

#[unsafe_destructor]
impl<'self> Drop for PreparedStatement<'self> {
    fn drop(&self) {
        unsafe { ffi::sqlite3_finalize(self.stmt); }
    }
}

impl<'self> PreparedStatement<'self> {
    fn reset(&self) {
        unsafe { ffi::sqlite3_reset(self.stmt); }
    }

    fn bind_params(&self, params: &[@SqlType]) -> Result<(), ~str> {
        for params.iter().enumerate().advance |(idx, param)| {
            let ret = do param.to_sql_str().as_c_str |c_param| {
                unsafe {
                    ffi::sqlite3_bind_text(self.stmt, (idx+1) as c_int,
                                           c_param, -1,
                                           ffi::SQLITE_TRANSIENT())
                }
            };

            if ret != ffi::SQLITE_OK {
                return Err(self.conn.get_error());
            }
        }

        Ok(())
    }
}

impl<'self> PreparedStatement<'self> {

    pub fn update(&self) -> Result<uint, ~str> {
        self.update_params([])
    }

    pub fn update_params(&self, params: &[@SqlType]) -> Result<uint, ~str> {
        self.reset();
        ret_err!(self.bind_params(params));

        let ret = unsafe { ffi::sqlite3_step(self.stmt) };

        match ret {
            // TODO: Should we consider a query that returned rows an error?
            ffi::SQLITE_DONE | ffi::SQLITE_ROW =>
                Ok(unsafe { ffi::sqlite3_changes(self.conn.conn) } as uint),
            _ => Err(self.conn.get_error())
        }
    }

    pub fn query(&'self self) -> Result<ResultIterator<'self>, ~str> {
        self.query_params([])
    }

    pub fn query_params(&'self self, params: &[@SqlType])
            -> Result<ResultIterator<'self>, ~str> {
        self.reset();
        ret_err!(self.bind_params(params));
        Ok(ResultIterator {stmt: self})
    }
}

pub struct ResultIterator<'self> {
    priv stmt: &'self PreparedStatement<'self>
}

impl<'self> Iterator<Row<'self>> for ResultIterator<'self> {
    fn next(&mut self) -> Option<Row<'self>> {
        let ret = unsafe { ffi::sqlite3_step(self.stmt.stmt) };
        match ret {
            ffi::SQLITE_ROW => Some(Row {stmt: self.stmt}),
            // TODO: Ignoring errors for now
            _ => None
        }
    }
}

pub struct Row<'self> {
    priv stmt: &'self PreparedStatement<'self>
}

impl<'self> Container for Row<'self> {
    fn len(&self) -> uint {
        unsafe { ffi::sqlite3_column_count(self.stmt.stmt) as uint }
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl<'self> Row<'self> {
    pub fn get<T: SqlType>(&self, idx: uint) -> Option<T> {
        let raw = unsafe {
            ffi::sqlite3_column_text(self.stmt.stmt, idx as c_int)
        };

        if ptr::is_null(raw) {
            return None;
        }

        SqlType::from_sql_str(unsafe { str::raw::from_c_str(raw) })
    }
}

pub trait SqlType {
    fn to_sql_str(&self) -> ~str;
    fn from_sql_str(sql_str: &str) -> Option<Self>;
}

impl SqlType for int {
    fn to_sql_str(&self) -> ~str {
        self.to_str()
    }

    fn from_sql_str(sql_str: &str) -> Option<int> {
        FromStr::from_str(sql_str)
    }
}