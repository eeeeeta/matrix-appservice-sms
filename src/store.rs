use diesel::prelude::*;
use pool::Pool;
use diesel::PgConnection;
use models::{Message, NewMessage, Recipient, NewRecipient};
use r2d2_diesel::ConnectionManager;
use huawei_modem::pdu::PduAddress;
use r2d2::PooledConnection;
use util;

type Result<T> = ::std::result::Result<T, ::failure::Error>;

#[derive(Clone)]
pub struct Store {
    pub(crate) inner: Pool
}
impl Store {
    fn get_conn(&mut self) -> PooledConnection<ConnectionManager<PgConnection>> {
        self.inner.get().expect("couldn't get a db connection!")
    }
    pub fn create_message(&mut self, r: NewMessage) -> Result<Message> {
        use schema::messages;
        let conn = self.get_conn();

        let res = ::diesel::insert_into(messages::table)
            .values(&r)
            .get_result(&*conn)?;
        Ok(res)
    }
    pub fn create_recipient(&mut self, r: NewRecipient) -> Result<Recipient> {
        use schema::recipients;
        let conn = self.get_conn();

        let res = ::diesel::insert_into(recipients::table)
            .values(&r)
            .get_result(&*conn)?;
        Ok(res)
    }
    pub fn set_recipient_for_messages(&mut self, rid: i32, msgs: &[i32]) -> Result<()> {
        use schema::messages::dsl::*;
        use diesel::pg::expression::dsl::any;
        let conn = self.get_conn();

        ::diesel::update(messages.filter(id.eq(any(msgs))))
            .set(recipient_id.eq(rid))
            .execute(&*conn)?;
        Ok(())
    }
    pub fn get_recipient_for_address(&mut self, addr: &PduAddress) -> Result<Option<Recipient>> {
        use schema::recipients::dsl::*;
        let conn = self.get_conn();
        let addr = util::normalize_address(addr);

        let res = recipients.filter(phone_number.eq(&addr))
            .first::<Recipient>(&*conn)
            .optional()?;
        Ok(res)
    }
    pub fn get_recipient_for_room(&mut self, rid: &str) -> Result<Option<Recipient>> {
        use schema::recipients::dsl::*;
        let conn = self.get_conn();

        let res = recipients.filter(room_id.eq(rid))
            .first::<Recipient>(&*conn)
            .optional()?;
        Ok(res)
    }
    pub fn get_recipients_with_messages(&mut self) -> Result<Vec<i32>> {
        use schema::recipients::dsl::*;
        use schema::messages;
        let conn = self.get_conn();
        let res = recipients.inner_join(messages::table)
            .filter(messages::dsl::processing.eq(false))
            .select(id)
            .load(&*conn)?;
        Ok(res)
    }
    pub fn get_orphaned_messages(&mut self) -> Result<Vec<Message>> {
        use schema::messages::dsl::*;
        let conn = self.get_conn();

        let res = messages.filter(recipient_id.is_null())
            .load(&*conn)?;
        Ok(res)
    }
    pub fn get_messages_for_recipient(&mut self, rid: i32) -> Result<Vec<Message>> {
        use schema::messages::dsl::*;
        let conn = self.get_conn();

        let res = messages.filter(recipient_id.eq(rid))
            .load(&*conn)?;
        Ok(res)
    }
    pub fn get_recipient_by_id(&mut self, mid: i32) -> Result<Recipient> {
        use schema::recipients::dsl::*;
        let conn = self.get_conn();

        let res = recipients.filter(id.eq(mid))
            .first(&*conn)?;
        Ok(res)
    }
    pub fn get_message_by_id(&mut self, mid: i32) -> Result<Message> {
        use schema::messages::dsl::*;
        let conn = self.get_conn();

        let res = messages.filter(id.eq(mid))
            .first(&*conn)?;
        Ok(res)
    }
    pub fn mark_message_processing(&mut self, mid: i32, proc: bool) -> Result<()> {
        use schema::messages::dsl::*;
        let conn = self.get_conn();

        let rows_affected = ::diesel::update(messages.filter(id.eq(mid)))
            .set(processing.eq(proc))
            .execute(&*conn)?;
        assert!(rows_affected > 0);
        Ok(())
    }
    pub fn mark_message_failed(&mut self, mid: i32) -> Result<i32> {
        use schema::messages::dsl::*;
        let conn = self.get_conn();

        let msg: Message = ::diesel::update(messages.filter(id.eq(mid)))
            .set((failures.eq(failures + 1), processing.eq(false)))
            .get_result(&*conn)?;
        Ok(msg.failures)
    }
    pub fn delete_message(&mut self, mid: i32) -> Result<()> {
        use schema::messages::dsl::*;
        let conn = self.get_conn();

        let rows_affected = ::diesel::delete(messages.filter(id.eq(mid)))
            .execute(&*conn)?;
        assert!(rows_affected > 0);
        Ok(())
    }
}
