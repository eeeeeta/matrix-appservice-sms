#![feature(plugin, proc_macro, conservative_impl_trait, generators)]
#![plugin(rocket_codegen)]

extern crate rocket;
extern crate rocket_contrib;
extern crate serde;
#[macro_use] extern crate serde_derive;
extern crate serde_json;
extern crate glitch_in_the_matrix as gm;
#[macro_use] extern crate diesel;
extern crate dotenv;
extern crate r2d2;
extern crate r2d2_diesel;
extern crate huawei_modem;
extern crate futures_await as futures;
extern crate tokio_core;
#[macro_use] extern crate failure;
#[macro_use] extern crate log;
extern crate env_logger;

mod pool;
mod future;
mod schema;
mod models;

use rocket::fairing::AdHoc;
use pool::DbConn;
use rocket_contrib::Json;
use gm::types::events::Events;

#[get("/")]
fn home() -> &'static str {
    "matrix-appservice-sms here, alive and well!"
}
#[put("/transactions/<id>", data = "<txn>")]
fn tx_put(id: String, txn: Json<Events>, db: DbConn) -> Json<()> {
    unimplemented!()
}
#[get("/users/<mxid>")]
fn user_query(mxid: String, db: DbConn) -> Json<()> {
    unimplemented!()
}
fn main() {
    rocket::ignite()
        .attach(AdHoc::on_attach(|rocket| {
            info!("Setting up database connection...");
            Ok(pool::attach_db(rocket))
        }))
        .attach(AdHoc::on_attach(|rocket| {
            info!("Setting up modem & Tokio thread...");
            Ok(future::attach_tokio(rocket))
        }))
        .mount("/",
               routes![home])
        .launch();
}
