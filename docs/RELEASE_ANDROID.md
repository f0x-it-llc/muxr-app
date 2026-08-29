# Muxr — Android Play Release Runbook

## 1. Prerequisites

A Google Play developer account. Check its type: Play Console → Setup → Account details.
Personal accounts created after 2023-11-13 need 12 testers opted in continuously for 14 days on
a closed track before production access (org-account exemption unconfirmed — the rule applies to
personal accounts; confirm your account type). See References.

## 2. Keystores

Generate `production.jks` (Ed only, never an agent): alias `muxr-upload`, RSA 4096
(`android/app/keystores/README.md`). Back up the `.jks` file and its passwords to the team
password manager immediately. Never regenerate `develop.jks`. A lost UPLOAD key is recoverable
via Play App Signing's upload-key reset request (App integrity → Play app signing → Request
upload key reset); a lost `develop.jks` has no reset and orphans dev installs.

## 3. Play Console app entries

TWO entries: `Muxr` (`com.it.f0x.muxr`, the product) and `Muxr Dev` (`com.it.f0x.muxr.dev`,
INTERNAL TESTING ONLY, never promoted). New apps publish as App Bundles, which use Play App
Signing — Google holds the app signing key; the AAB you upload is signed with the upload key.

## 4. In-app product

Monetize with Play → Products → In-app products → create `muxr_pro` (one-time, non-consumable):
name, description, price, then Activate — in BOTH entries (products are per app entry). A build
carrying the BILLING permission (pulled in transitively by `frust-iap`) must have been uploaded
once to an entry before its products can be created. Allow time for a new product to become
available; verify with a fresh fetch before relying on it.

## 5. License testers

Setup → License testing: add every tester's Google account. Test instruments: always approves /
always declines / slow (approves or declines after a few minutes) / approves-then-charges-back.
License-tester purchases auto-refund after **3 MINUTES** if unacknowledged (real purchases: 3
days). Muxr acknowledges immediately on `PurchaseUpdated`, so a Free flip ~3 minutes after a
tester purchase is a bug, not expected behavior.

## 6. Builds

Develop (sideload; signed with `develop.jks` via `key.properties` — a debug-signed build would
also work for license testers):

```bash
(cd android && ./gradlew :app:assembleDevelopRelease)
adb install -r android/app/build/outputs/apk/develop/release/app-develop-release.apk
```

License testers can purchase from this sideloaded build — even debug-signed — as long as the
package name (`com.it.f0x.muxr.dev`) matches the Play Console app entry and the account is a
license tester; no track build is required for that.

Production AAB:

```bash
frust build appbundle --release --flavor production --build-name <semver> --build-number <N>
```

writes `android/app/build/outputs/bundle/productionRelease/app-production-release.aab`.
`--build-number` (`versionCode`) must increase monotonically per upload. 16 KB page check before
upload:

```bash
llvm-readelf -lW android/app/src/main/jniLibs/arm64-v8a/libmuxr.so | grep LOAD
```

every segment's `Align` must read `0x4000`. The Gradle warning `[muxr] release variant
productionRelease is DEBUG-SIGNED` must **not** appear — its presence means `prod.*` keys
weren't found in `android/app/keystores/key.properties` (see `docs/DEVELOPMENT.md`'s Android
Signing section).

## 7. Internal testing

Upload an AAB to each entry's internal track; add up to 100 testers; share the opt-in link —
available within minutes. Follow whatever review/declaration prompt Play Console shows on the
first upload to a new app entry; do not assume an exemption.

## 8. Device gate (G1–G10)

Run on the Play INTERNAL track (`.dev` entry, license tester account, sideloaded
`assembleDevelopRelease`) except G9, which uses the minified production build.

| Gate | Check | Result |
|---|---|---|
| G1 | Free tier blocks a second endpoint; 'Forget A and connect' converges | |
| G2 | always-approves purchase → Pro, sheet closes, dial proceeds, still Pro after > 3 minutes (acknowledged) | |
| G3 | uninstall/reinstall → Pro restored at launch (and via Restore purchases) | |
| G4 | kill mid-flow, complete in Play, relaunch → granted + acknowledged | |
| G5 | slow-approves card → Pending copy → later Pro | |
| G6 | Play Console refund → next refresh → Free | |
| G7 | always-declines → failure toast, Free | |
| G8 | airplane-mode launch → cached Pro stands | |
| G9 | the minified production build (`com.it.f0x.muxr` entry) completes a purchase | |
| G10 | no plugin panic/ANR in logcat across G1–G9 (`adb logcat \| grep -i 'iap\|billing'`) | |

## 9. Production readiness

Privacy policy URL hosted at muxr.app (active, public, non-geofenced). Data safety form: the app
stores server endpoints + tokens on-device only; nothing collected/shared; purchases are handled
by Google Play. Content rating: complete the IARC questionnaire — an unrated app is removed. App
access: sign-in required — provide an always-accessible, reusable `muxrd` endpoint + read-only
token, in English. Target audience: set; ads: none. Declarations: government/financial/health/news
= no; account deletion = not applicable (no accounts). Store listing assets: icon 512×512 32-bit
PNG ≤ 1 MB; feature graphic 1024×500; ≥ 2 phone screenshots (4+ recommended), 320–3840 px; short
description ≤ 80 chars; full description ≤ 4000 chars. Satisfy the closed-test window first if
the account is personal (§1), then submit the production rollout (staged % optional).

## 10. Deadlines already met

Target API 36 and Play Billing Library ≥ 8, both due 2026-08-31: Muxr ships `targetSdk 36` and
PBL 9.1.0 via `openiap-google` 3.0.1. 16 KB page size: updates without support are cut off
starting 2027-02-01. Any future `openiap` pin bump must keep both floors and stay lockstep with
the iOS revision (`plugins/iap/platform/android/build.gradle.kts`'s `openiap-google` version and
`plugins/iap/platform/ios`'s SPM revision).

## References

- Closed-test rule (personal accounts) — https://support.google.com/googleplay/android-developer/answer/14151465
- Account type — https://support.google.com/googleplay/android-developer/answer/13634885
- License testing — https://support.google.com/googleplay/android-developer/answer/6062777
- Test instruments, sideload testing, 3-minute refund — https://developer.android.com/google/play/billing/test
- One-time purchase lifecycle (3-day refund) — https://developer.android.com/google/play/billing/lifecycle/one-time
- Target API level — https://support.google.com/googleplay/android-developer/answer/11926878
- Play Billing Library deprecation — https://developer.android.com/google/play/billing/deprecation-faq
- 16 KB page size — https://developer.android.com/guide/practices/page-sizes
- Play App Signing / upload key reset — https://support.google.com/googleplay/android-developer/answer/9842756
- In-app products — https://support.google.com/googleplay/android-developer/answer/1153481
- Internal testing — https://support.google.com/googleplay/android-developer/answer/9845334
- Content rating — https://support.google.com/googleplay/android-developer/answer/9898843
- App access — https://support.google.com/googleplay/android-developer/answer/15748846
- Data safety + privacy policy — https://support.google.com/googleplay/android-developer/answer/10787469
- Store listing assets — https://support.google.com/googleplay/android-developer/answer/9866151
- Full description length — https://support.google.com/googleplay/android-developer/answer/13393723
