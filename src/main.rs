#![feature(plugin, try_from, proc_macro, custom_derive, conservative_impl_trait, generators, advanced_slice_patterns, slice_patterns)]
#![plugin(rocket_codegen)]

extern crate rocket;
extern crate rocket_contrib;
extern crate serde;
#[macro_use] extern crate serde_json;
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
use future::{MessagingHandle, IntMessage};
use rocket_contrib::Json;
use serde_json::Value;
use rocket::http::Status;
use rocket::response::status;
use gm::types::replies::BadRequestReply;
use gm::types::events::{Event, Events};
use diesel::prelude::*;
use models::PutRequest;

#[get("/")]
fn home() -> &'static str {
    "matrix-appservice-sms here, alive and well!"
}
#[derive(FromForm)]
struct HsToken {
    access_token: String
}
#[put("/transactions/<txnid>?<access>", data = "<txn>")]
fn tx_put(txnid: String, access: HsToken, txn: Json<Events>, db: DbConn, hdl: MessagingHandle) -> Result<status::Custom<Json<Value>>, failure::Error> {
    if access.access_token != hdl.hs_token {
        let val = serde_json::to_value(BadRequestReply {
            errcode: "M_FORBIDDEN".into(),
            error: None
        }).unwrap_or(json! {{ }});
        return Ok(status::Custom(Status::Forbidden, Json(val)));
    }
    let pr = {
        use schema::putrequests::dsl::*;

        putrequests.filter(id.eq(&txnid))
            .first::<PutRequest>(&*db)
            .optional()?
    };
    if let Some(PutRequest { id }) = pr {
        // already processed
        info!("Already processed txn #{}.", id);
        return Ok(status::Custom(Status::Ok, Json(json! {{ }})))
    }
    for evt in txn.0.events {
        if let Event::Full(meta, content) = evt {
            hdl.tx.unbounded_send(IntMessage::MatrixEvent(meta, content))
                .expect("failed to send events to future");
        }
    }
    {
        use schema::putrequests;
        let new_pr = PutRequest { id: txnid };
        diesel::insert_into(putrequests::table)
            .values(&new_pr)
            .execute(&*db)?;
    }
    Ok(status::Custom(Status::Ok, Json(json! {{ }})))
}
#[put("/transactions/<_id>", rank = 10)]
fn tx_put_nocreds(_id: String) -> status::Custom<Json<BadRequestReply>> {
    status::Custom(Status::Unauthorized,
                   Json(BadRequestReply {
                       errcode: "org.eu.theta.non_compliant".into(),
                       error: None
                   }))
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
               routes![home, tx_put, tx_put_nocreds])
        .launch();
}
