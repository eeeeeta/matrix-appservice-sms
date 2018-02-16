use schema::{putrequests, recipients, users};

#[derive(Insertable, Queryable)]
#[table_name="putrequests"]
pub struct PutRequest {
    pub id: String
}
#[derive(Queryable)]
pub struct Recipient {
    pub id: i32,
    pub phone_number: String,
    pub user_id: String,
    pub room_id: String
}
#[derive(Insertable)]
#[table_name="recipients"]
pub struct NewRecipient<'a> {
    pub phone_number: &'a str,
    pub user_id: &'a str,
    pub room_id: &'a str
}
#[derive(Queryable)]
pub struct User {
    pub id: i32,
    pub mxid: String,
    pub config_room_id: Option<String>
}
#[derive(Insertable)]
#[table_name="users"]
pub struct NewUser<'a> {
    pub mxid: &'a str
}
