# Contributing to DropBridge

Thanks for helping. Bug reports, fixes and docs are all welcome.

## Before you start

- **Bugs:** open an issue with the bug template. Include the output of `dropbridge doctor`.
- **Features:** open an issue first so we can agree on the idea before you write code.
- **Security problems:** don't open an issue. Report them privately from the [Security tab](https://github.com/Aayushkalikote/DropBridge/security/advisories/new).

## Making a change

1. Fork the repo and create a branch in your fork.
2. Build and test:

   ```sh
   brew install protobuf
   cargo test
   cargo run --release -- --test-peer        # Mac side without a phone
   RUST_LOG=dropbridge=debug cargo run --release
   ```

3. Test on real hardware when your change touches AirDrop or Quick Share: a Mac and an Android phone on the same Wi-Fi. Say in the PR what you tested on.
4. Add a changelog entry under a new version heading in the README's **Changelog** section. Start it with `Breaking:`, `Feature:`, `Enhancement:`, `Fix:`, `Security:` or `Docs:`.
5. Open a pull request against `main`. CI must pass before it can be merged.

Keep each PR to one change.

## License

DropBridge is GPL-3.0-only. By sending a pull request, you agree that your contribution is licensed under the same terms.
