# TF2 builds: tables and CRC

SourceTV sends no class tables: the client builds them from its game DLL and proves it with
a SendTable CRC at signon. The tables come from any demo of the build, and the CRC from them
(the engine's `SendTable_ComputeCRC`, over each server class's send table). Known builds are
compiled in (`BUILDS` in `src/builds.rs`, tables in `assets/`); the newest is `builds::CURRENT`.

## Which build is offered

The server refuses any build but its own. Each connection attempt offers, in this order:

1. the build given with `--build N`; the server is then not asked;
2. the build in the STV port's A2S_INFO reply (7 to 10 digits), shown in the `connecting` status;
3. without a usable reply, the last build it offered, or the current one.

A build from the server's reply that stvtf has no data for gets the current build's tables, since
most TF2 updates keep them. When an update does change them, the server answers "Server uses
different class tables". An unknown build given with `--build` is an error unless `--tables` is
given too.

## Another build

Pass `--tables` (any STV or POV demo of that build). `--crc` overrides the CRC its tables give
(hex, with or without `0x`). To compile a build in:

```
cargo run --example add_build -- DEMO BUILD
```

DEMO is a `.dem` file, a demos.tf link (`https://demos.tf/1234567`) or a URL of a `.dem`; BUILD
is the `build` of the `connecting` status. A build with the same tables as a known one shares
them; otherwise the demo is cut after its data tables into `assets/tables_BUILD.dem`. Links and
URLs are fetched with `curl`. A build already listed is refused.
