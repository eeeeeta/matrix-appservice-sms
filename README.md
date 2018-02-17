# matrix-appservice-sms

## What is this?

This is a Matrix.org [Application Service (AS)](https://matrix.org/docs/spec/application_service/unstable.html) that bridges SMS messages
to Matrix, using a USB 3G modem plugged into a server. Crucially, **it only works for one user** - obviously, if you only have one 3G modem,
it doesn't really make sense to support *more* than one user...

## What do I need to run it?

- an instance of PostgreSQL
- a Huawei USB 3G modem, like the Huawei E3531, or Huawei E169
  - Other models of 3G modem are available. Essentially, if it uses the AT/Hayes command set, and implements a bunch of common
    commands, it'll probably work.
  - This has been tested working with the Huawei E3531, though.
- a **nightly** version of [Rust](http://rust-lang.org/); see below for details
- a decent amount of patience ;p

## How do I set this up?

Oh, man.

### Set up the 3G modem

- Firstly, ensure that your 3G modem presents itself as a serial port - usually with a path like `/dev/ttyUSBX`.
  - Advice is available [on the Arch Wiki](https://wiki.archlinux.org/index.php/USB_3G_Modem), and varies from model to model. You
    might need to do a bit of googling to figure out how to poke your modem into appearing as a serial device, as many appear as
    mass storage devices with software by default.
  - For the Huawei E3531, try `sudo usb_modeswitch -v 12d1 -p 15cd -M '55534243123456780000000000000011062000000100000000000000000000'`.
    - If that doesn't work, try changing the `12d1` and `15cd` above with whatever `VID:PID` combo appears next to the device in `lsusb`.
    - For example, if your device presents itself as a `ID 12d1:1506 Huawei Technologies Co., Ltd. Modem/Networkcard` when `lsusb` is run,
      you'd use `-v 12d1` and `-p 1506`.
    - Again, this stuff is really dependent on your modem. Google is your friend, if the above device didn't work for you!
  - Some modems (like the E3531) present *three* serial ports: `/dev/ttyUSB0-2`. You want `/dev/ttyUSB2` in this case.
  - Once you've found the device path for your modem, try connecting to it with `sudo minicom -D /dev/ttyUSB2` (replacing `/dev/ttyUSB2`
    with the path for your modem if different), and typing `AT<Enter>`. If you get `OK` back, you've found the right path!
- Also ensure that the user you're going to run the AS as can read and write to this serial port. (`sudo chmod 777 /dev/ttyUSB2` does the job,
  but is pretty bad security-wise; it's better to configure users and groups correctly.)

### Set up Rust

- You need the latest *nightly* version of Rust installed; check out [their install page](https://www.rust-lang.org/en-US/install.html).
  If you install with `rustup`, change the default configuration when prompted to install the nightly toolchain.
- Build the project with `cargo build`.
- Then, install `diesel_cli` with `cargo install diesel_cli`.

### Set up the database

- Run `DATABASE_URL=[url] diesel migration run`, substituting `[url]` for your database URL (e.g. `postgresql://localhost/database`).

### Set up synapse

- Replace the placeholders in `sms-registration.yaml` with sane values (for the tokens and id, generate some random crap, but make sure
  all of the values are different; for the URL, put the URL of where you're going to put your matrix-appservice-sms instance).
- Copy this somewhere (usually `/etc/synapse`) readable by synapse.
- Edit `app_service_config_files` in your synapse `homeserver.yaml`, and add the path to your `sms-registration.yaml` file. *e.g.:*

```
app_service_config_files: ["/etc/synapse/sms-registration.yaml"]
```

- (Hold off on restarting synapse for now; we want to make sure that the AS is reachable first.)

### Set up the AS

- Replace the placeholders in `Rocket.example.toml`, following the guidance therein, and rename it to `Rocket.toml`.
- Run the AS with `cargo run`.
- Now, restart synapse.

### Use the AS

- Invite `@_sms_bot:[your HS]` to a 1-1 chat.
- Issue `!sms [phone number]` in that chatroom to start texting! (You'll get invited to a private chat with that phone number.) Make sure the number is
  in international format (e.g. +440123456789) for best results.

## Your setup instructions are crap! / It didn't work!

- Yell at me on Matrix, then: [@eta:theta.eu.org](https://matrix.to/#/@eta:theta.eu.org).

## License

- This is GPLv3-licensed software.
