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
    users (id) {
        id -> Int4,
        mxid -> Varchar,
        config_room_id -> Nullable<Varchar>,
    }
}
