use schema::{putrequests, recipients, messages};

#[derive(Insertable, Queryable)]
#[table_name="putrequests"]
pub struct PutRequest {
    pub id: String
}
#[derive(Identifiable, Queryable)]
#[table_name="recipients"]
pub struct Recipient {
    pub id: i32,
    pub phone_number: String,
    pub user_id: String,
    pub room_id: String
}
#[derive(Associations, Identifiable, Queryable)]
#[table_name = "messages"]
#[belongs_to(Recipient, foreign_key = "recipient_id")]
pub struct Message {
    pub id: i32,
    pub recipient_id: Option<i32>,
    pub pdu: Vec<u8>,
    pub processing: bool,
    pub failures: i32
}
#[derive(Insertable)]
#[table_name="messages"]
pub struct NewMessage<'a> {
    pub recipient_id: Option<i32>,
    pub pdu: &'a [u8]
}
#[derive(Insertable)]
#[table_name="recipients"]
pub struct NewRecipient<'a> {
    pub phone_number: &'a str,
    pub user_id: &'a str,
    pub room_id: &'a str
}

