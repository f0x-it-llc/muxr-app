# Android release keystores

This directory holds the release keystores and `key.properties` **locally only** —
everything except this README, the `.gitignore`, and `key.properties.example` is
gitignored. Do not force-add keystores or `key.properties` to git.

## One-time setup

1. Generate the two keystores (pick strong, distinct passwords; store them in the
   team password manager together with the `.jks` files themselves):

   ```bash
   keytool -genkeypair -v \
     -keystore develop.jks -alias muxr-dev \
     -keyalg RSA -keysize 4096 -validity 10000

   keytool -genkeypair -v \
     -keystore production.jks -alias muxr-upload \
     -keyalg RSA -keysize 4096 -validity 10000
   ```

2. `cp key.properties.example key.properties` and fill in the passwords.

3. Verify: `cd android && ./gradlew :app:assembleProductionRelease` then
   `apksigner verify --print-certs app/build/outputs/apk/production/release/app-production-release.apk`.

## CI

CI uses env vars instead of `key.properties` (see `android/app/build.gradle.kts`):
`DEVELOP_KEYSTORE_PATH` / `DEVELOP_STORE_PASSWORD` / `DEVELOP_KEY_ALIAS` /
`DEVELOP_KEY_PASSWORD` and `KEYSTORE_PATH` / `STORE_PASSWORD` / `KEY_ALIAS` /
`KEY_PASSWORD`. Store the keystore base64-encoded in a CI secret and decode it to
a temp file at build time.

## Recovery

If you enroll in **Play App Signing** (recommended), Google holds the app signing
key and `production.jks` is only the *upload* key — losing it is recoverable via a
key-reset request. Without Play App Signing, losing `production.jks` permanently
orphans the app ID. Back it up accordingly.
