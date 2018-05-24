table! {
    putrequests (id) {
        id -> Varchar,
    }
}

table! {
    recipients (id) {
        id -> Int4,
        phone_number -> Varchar,
        user_id -> Varchar,
        room_id -> Varchar,
    }
}

table! {
    messages (id) {
        id -> Int4,
        recipient_id -> Nullable<Int4>,
        pdu -> Bytea,
        processing -> Bool,
        failures -> Int4,
    }
}
joinable!(messages -> recipients (recipient_id));
allow_tables_to_appear_in_same_query!(recipients, messages);
