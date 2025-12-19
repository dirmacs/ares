#!/usr/bin/env nu
# Run the Hurl suite against a running A.R.E.S server.
# Usage:
#   ./scripts/hurl/run.nu
# Optional env overrides:
#   $env.ARES_BASE_URL = "http://127.0.0.1:3000"
#   $env.ARES_TEST_EMAIL = "..."
#   $env.ARES_TEST_PASSWORD = "..."

let base = ($env.ARES_BASE_URL? | default "http://127.0.0.1:3000")
print $"Running Hurl suite against ($base)"

let test_email = ($env.ARES_TEST_EMAIL? | default "hurl.user1@example.com")
let test_password = ($env.ARES_TEST_PASSWORD? | default "correcthorsebatterystaple")
let test_name = ($env.ARES_TEST_NAME? | default "Hurl User")

let cases = [
  "hurl/cases/00_health.hurl"
  "hurl/cases/01_agents.hurl"
  "hurl/cases/10_auth_register_login_refresh.hurl"
  "hurl/cases/11_auth_negative.hurl"
  "hurl/cases/20_chat_and_memory.hurl"
  "hurl/cases/21_research.hurl"
  "hurl/cases/22_protected_negative.hurl"
  "hurl/cases/30_workflows.hurl"
  "hurl/cases/40_toon_import_export.hurl"
]

mut failed = false
for c in $cases {
  print $"\n==> ($c)"
  let r = (do {
  ^hurl --test $c --variable $"base_url=($base)" --variable $"test_email=($test_email)" --variable $"test_password=($test_password)" --variable $"test_name=($test_name)"
  } | complete)
  if $r.exit_code != 0 {
    $failed = true
    print $"FAILED: ($c)"
    print $r.stderr
  } else {
    print $"OK: ($c)"
  }
}

if $failed {
  error make { msg: "One or more Hurl cases failed" }
}

print "\nAll Hurl cases passed"
