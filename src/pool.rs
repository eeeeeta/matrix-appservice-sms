//! Managing the database pool.
//!
//! Code here abridged from https://rocket.rs/guide/state/#databases.

use diesel::PgConnection;
use r2d2_diesel::ConnectionManager;
use r2d2;
use rocket::Rocket;
use std::ops::Deref;
use rocket::http::Status;
use rocket::request::{self, FromRequest};
use rocket::{Request, State, Outcome};
use std::sync::Arc;

pub type Pool = Arc<r2d2::Pool<ConnectionManager<PgConnection>>>;

pub fn attach_db(rocket: Rocket) -> Rocket {
    let manager: ConnectionManager<PgConnection> = {
        let url = rocket.config().get_str("database_url")
            .expect("'database_url' in config");
        ConnectionManager::new(url)
    };
    let pool = r2d2::Pool::builder().build(manager).expect("db pool");
    rocket.manage(Arc::new(pool))
}

pub struct DbConn(pub r2d2::PooledConnection<ConnectionManager<PgConnection>>);

/// Attempts to retrieve a single connection from the managed database pool. If
/// no pool is currently managed, fails with an `InternalServerError` status. If
/// no connections are available, fails with a `ServiceUnavailable` status.
impl<'a, 'r> FromRequest<'a, 'r> for DbConn {
    type Error = ();

    fn from_request(request: &'a Request<'r>) -> request::Outcome<DbConn, ()> {
        let pool = request.guard::<State<Pool>>()?;
        match pool.get() {
            Ok(conn) => Outcome::Success(DbConn(conn)),
            Err(_) => Outcome::Failure((Status::ServiceUnavailable, ()))
        }
    }
}

impl Deref for DbConn {
    type Target = PgConnection;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
