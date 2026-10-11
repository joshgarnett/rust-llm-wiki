#!/bin/sh
# Install a pinned native release into an owner-controlled prefix.
# No Python, jq, root access, shell profile edits, or quarantine removal.
set -eu
LC_ALL=C
export LC_ALL
umask 077

fail() { printf 'lwiki install: %s\n' "$*" >&2; exit 1; }
help() {
    cat <<'EOF'
Usage: sh install.sh --version VERSION [--prefix DIR] [--from-dir DIR] [--dry-run]
       sh install.sh --rollback VERSION [--prefix DIR] [--dry-run]
       sh install.sh --uninstall [--prefix DIR] [--dry-run]

Install a fixed release of lwiki. The default prefix is $HOME/.local.
Supported targets: aarch64-apple-darwin and x86_64-unknown-linux-gnu.
Platform inspection requires otool and sw_vers on macOS, or GNU readelf and
getconf on GNU/Linux. Common tools: awk, iconv, tar, gzip, dd, od, stat and a
SHA256 utility. Each package declares its minimum OS or glibc version.
Online installation requires curl 8.4 or newer. --from-dir reads local release
assets: release-manifest.txt and lwiki-VERSION-TARGET.tar.gz.
--dry-run verifies inputs and prints the exact selected target without activation.
Rollback verifies and activates one retained version. Uninstall removes verified
launch links and keeps retained versions. Vaults and user config remain unchanged.

The prefix must be owned by your user and must not be writable by other users.
Symlink path components are refused. Installs use a private managed directory and
an exclusive lock. A stale lock needs manual inspection; it is never removed
automatically. This does not protect against malicious processes running as your
user or guarantee recovery from power loss.

macOS releases are unsigned and are not notarized. This installer preserves an
archive's quarantine attribute and does not disable Gatekeeper. HTTPS and release
hashes verify channel integrity, not independent publisher authentication.
Support: https://github.com/joshgarnett/rust-llm-wiki/issues
EOF
}
mode=install
version=
prefix=${HOME:?HOME is required}/.local
from_dir=
dry=false
seen_action=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --version|--rollback)
            [ "$#" -ge 2 ] || fail "$1 needs a version"
            [ "$seen_action" = false ] || fail 'select exactly one action'
            seen_action=true
            [ "$1" != --rollback ] || mode=rollback
            version=$2; shift 2 ;;
        --uninstall)
            [ "$seen_action" = false ] || fail 'select exactly one action'
            seen_action=true; mode=uninstall; shift ;;
        --prefix|--from-dir)
            [ "$#" -ge 2 ] || fail "$1 needs a directory"
            if [ "$1" = --prefix ]; then prefix=$2; else from_dir=$2; fi
            shift 2 ;;
        --dry-run) dry=true; shift ;;
        --help|-h) help; exit 0 ;;
        *) fail "unknown option: $1" ;;
    esac
done
[ "$seen_action" = true ] || fail 'an explicit --version, --rollback, or --uninstall is required'
[ "$mode" = install ] || [ -z "$from_dir" ] || fail '--from-dir applies only to installation'
if [ "$mode" != uninstall ]; then
    printf '%s\n' "$version" | awk 'length($0)<=80 && /^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$/ {ok=1} END {exit !ok}' || fail 'invalid version'
fi
case "$prefix" in /*) ;; *) fail 'prefix must be an absolute path' ;; esac
case "$prefix" in *'
'*|*'\t'*|*/../*|*/./*|*//*|*/..|*/.) fail 'prefix contains unsupported path components' ;; esac
prefix=${prefix%/}
[ -n "$prefix" ] || fail 'the filesystem root is not an installation prefix'
uid=$(id -u)
[ "$uid" != 0 ] || fail 'run as your own user, without sudo'
platform=$(uname -s)
arch=$(uname -m)
case "$platform:$arch" in
    Darwin:arm64|Darwin:aarch64) target=aarch64-apple-darwin; file_unit=1024 ;;
    Linux:x86_64|Linux:amd64) target=x86_64-unknown-linux-gnu; file_unit=512 ;;
    *) fail "unsupported native platform: $platform $arch" ;;
esac
if [ "$platform" = Darwin ]; then
    stat_fields() { stat -f '%u %Lp' "$1"; }
    hash() { shasum -a 256 "$1" | awk '{print $1}'; }
    hash_stdin() { shasum -a 256 | awk '{print $1}'; }
    replace_link() { mv -h "$1" "$2"; }
else
    stat_fields() { stat -c '%u %a' "$1"; }
    hash() { sha256sum "$1" | awk '{print $1}'; }
    hash_stdin() { sha256sum | awk '{print $1}'; }
    replace_link() { mv -T "$1" "$2"; }
fi
regular() { [ -f "$1" ] && [ ! -L "$1" ]; }
length() { wc -c < "$1" | tr -d ' '; }
check_dir() {
    [ ! -L "$1" ] && [ -d "$1" ] || fail "not a regular directory: $1"
    fields=$(stat_fields "$1") || fail "cannot stat: $1"
    owner=${fields%% *}; perms=${fields#* }
    if [ "$owner" != "$uid" ]; then
        [ "$2" = ancestor ] && [ "$owner" = 0 ] || fail "untrusted directory owner: $1"
    fi
    printf '%s\n' "$perms" | awk '/^[0-7]+$/ {m=$0; g=substr(m,length(m)-1,1); o=substr(m,length(m),1); if(g !~ /[2367]/ && o !~ /[2367]/) ok=1} END {exit !ok}' || {
        [ "$2" = ancestor ] && [ "$owner" = 0 ] && [ "$perms" = 1777 ] || fail "directory is writable by other users: $1"
    }
}
# Reject every symlink component. Root-owned sticky temporary ancestors are safe
# only above a private, user-owned prefix, not as the prefix itself.
check_prefix() {
    probe=$prefix
    if [ ! -e "$probe" ] && [ ! -L "$probe" ]; then probe=${probe%/*}; [ -n "$probe" ] || probe=/; fi
    while :; do
        if [ "$probe" = "$prefix" ]; then check_dir "$probe" prefix; else check_dir "$probe" ancestor; fi
        [ "$probe" != / ] || break
        probe=${probe%/*}; [ -n "$probe" ] || probe=/
    done
}
check_prefix
managed=$prefix/.lwiki-install
launch=$prefix/bin/lwiki
owner_text="lwiki.install-owner.v1 $uid $(printf '%s' "$prefix" | hash_stdin)"
locked=false
stage=
bootstrap=
cleanup() {
    code=$?
    trap - EXIT HUP INT TERM
    if [ -n "$stage" ] && [ -d "$stage" ] && [ ! -L "$stage" ]; then
        if [ -d "$stage/generation" ] && [ ! -L "$stage/generation" ]; then
            for name in lwiki version.txt capabilities.json build-info.json LICENSE THIRD-PARTY-NOTICES INSTALL.txt artifact-info.txt release-manifest.txt receipt; do
                regular "$stage/generation/$name" && rm -f "$stage/generation/$name" || :
            done
            rmdir "$stage/generation" 2>/dev/null || :
        fi
        # Remove only fixed files created by this invocation. Unknown evidence stays.
        for name in lwiki version.txt capabilities.json build-info.json LICENSE THIRD-PARTY-NOTICES INSTALL.txt artifact-info.txt release-manifest.txt receipt archive.tar.gz archive.tar header header.bytes headers rows json.out json.utf8 platform.out platform.version platform.host version.out version.err bounded.err quarantine next-current next-launch; do
            if regular "$stage/$name" || [ -L "$stage/$name" ]; then rm -f "$stage/$name" || :; fi
        done
        rmdir "$stage" 2>/dev/null || printf 'lwiki install: retained staging evidence: %s\n' "$stage" >&2
    fi
    if [ "$locked" = true ]; then
        if regular "$managed/.lock/owner" && [ "$(cat "$managed/.lock/owner")" = "$$ $uid" ]; then
            rm "$managed/.lock/owner" && rmdir "$managed/.lock" || :
        fi
    fi
    if [ -n "$bootstrap" ] && [ -d "$bootstrap" ] && [ ! -L "$bootstrap" ]; then
        candidate=$bootstrap/.lwiki-install
        if [ -d "$candidate" ] && [ ! -L "$candidate" ]; then
            # Only this invocation's exact bootstrap marker and empty directories
            # are disposable. Changed or additional material remains for inspection.
            for name in .owner .owner.pending; do
                if regular "$candidate/$name" && [ "$(length "$candidate/$name")" -le 200 ] && [ "$(cat "$candidate/$name"; printf '.')" = "$owner_text
." ]; then rm "$candidate/$name" || :; fi
            done
            if [ -d "$candidate/versions" ] && [ ! -L "$candidate/versions" ]; then rmdir "$candidate/versions" 2>/dev/null || :; fi
            rmdir "$candidate" 2>/dev/null || :
        fi
        rmdir "$bootstrap" 2>/dev/null || printf 'lwiki install: retained bootstrap evidence: %s\n' "$bootstrap" >&2
    fi
    exit "$code"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

check_owned_marker() {
    check_prefix
    check_dir "$managed" prefix
    [ "$(stat_fields "$managed")" = "$uid 700" ] || fail 'managed directory must have mode 700'
    regular "$managed/.owner" && [ "$(stat_fields "$managed/.owner")" = "$uid 600" ] && [ "$(length "$managed/.owner")" -le 200 ] && [ "$(cat "$managed/.owner"; printf '.')" = "$owner_text
." ] || fail 'unknown managed directory owner marker'
}
check_managed() {
    check_owned_marker
    check_dir "$managed/versions" prefix
    for entry in "$managed"/* "$managed"/.[!.]* "$managed"/..?*; do
        [ -e "$entry" ] || [ -L "$entry" ] || continue
        case "${entry##*/}" in .owner|versions|current|.lock|.stage) ;; *) fail "unknown managed path: $entry" ;; esac
    done
}
if [ -e "$managed" ] || [ -L "$managed" ]; then
    if [ ! -e "$managed/versions" ] && [ ! -L "$managed/versions" ]; then
        # Reconcile only the old, exact owner-marker-only interruption state.
        # A lock, launch, current, staging path or any unknown entry is not proof.
        check_owned_marker
        [ "$mode" = install ] || fail 'owned initialization is incomplete; retry with --version to finish it'
        for entry in "$managed"/* "$managed"/.[!.]* "$managed"/..?*; do
            [ -e "$entry" ] || [ -L "$entry" ] || continue
            [ "${entry##*/}" = .owner ] || fail "unknown incomplete initialization path: $entry"
        done
        [ ! -e "$launch" ] && [ ! -L "$launch" ] || fail 'incomplete initialization has an unexpected launch path'
        [ "$dry" = false ] || fail 'owned initialization is incomplete; retry installation without --dry-run to finish it'
        mkdir "$managed/versions" || fail 'initialization changed concurrently; retry installation'
    fi
    check_managed
else
    [ "$mode" = install ] || fail 'no managed installation exists'
fi
if [ -e "$launch" ] || [ -L "$launch" ]; then
    [ -L "$launch" ] && [ "$(readlink "$launch"; printf '.')" = '../.lwiki-install/current/lwiki
.' ] || fail 'refusing to replace an unknown launch path'
    [ -L "$managed/current" ] || fail 'owned launch link has no current generation'
fi
if [ -e "$prefix/bin" ] || [ -L "$prefix/bin" ]; then check_dir "$prefix/bin" prefix; fi
if [ "$dry" = false ]; then
    if [ ! -e "$prefix" ]; then mkdir "$prefix"; check_dir "$prefix" prefix; fi
    if [ ! -e "$managed" ]; then
        bootstrap=$(mktemp -d "$prefix/.lwiki-bootstrap.XXXXXX") || fail 'cannot create private bootstrap directory'
        mkdir "$bootstrap/.lwiki-install"
        printf '%s\n' "$owner_text" > "$bootstrap/.lwiki-install/.owner.pending"
        mv "$bootstrap/.lwiki-install/.owner.pending" "$bootstrap/.lwiki-install/.owner"
        mkdir "$bootstrap/.lwiki-install/versions"
        check_prefix
        [ ! -e "$managed" ] && [ ! -L "$managed" ] || fail 'installation initialized concurrently; retry installation'
        # Source has the final basename. Moving into its parent computes the final
        # path once and refuses a concurrent nonempty managed directory.
        mv "$bootstrap/.lwiki-install" "$prefix"
        [ ! -e "$bootstrap/.lwiki-install" ] || fail 'managed bootstrap was not published'
        rmdir "$bootstrap"
        bootstrap=
    fi
    check_managed
    mkdir "$managed/.lock" 2>/dev/null || fail "installation lock exists: $managed/.lock; inspect it and remove it only when no installer is running"
    locked=true
    printf '%s\n' "$$ $uid" > "$managed/.lock/owner"
    [ ! -e "$managed/.stage" ] && [ ! -L "$managed/.stage" ] || fail 'staging evidence exists; inspect it before retrying'
    mkdir "$managed/.stage"; stage=$managed/.stage
else
    stage=$(mktemp -d "${TMPDIR:-/tmp}/lwiki-install.XXXXXX") || fail 'cannot create private staging directory'
fi

# Producer exit status is checked directly. There is no tar-to-head pipeline.
bounded() {
    destination=$1; bound=$2; seconds=$3; shift 3
    blocks=$(( (bound + file_unit - 1) / file_unit ))
    (ulimit -f "$blocks"; exec "$@") > "$destination" 2> "$stage/bounded.err" &
    producer=$!
    (sleep "$seconds"; kill -TERM "$producer" 2>/dev/null || :; sleep 1; kill -KILL "$producer" 2>/dev/null || :) > /dev/null 2>&1 &
    timer=$!
    status=0; wait "$producer" || status=$?
    kill "$timer" 2>/dev/null || :; wait "$timer" 2>/dev/null || :
    [ "$status" = 0 ] || fail "bounded command failed (exit $status): $1"
    [ "$(length "$destination")" -le "$bound" ] || fail "output exceeds byte limit: $destination"
}

normalize_platform_version() {
    printf '%s\n' "$1" | awk -v kind="$2" '
      NR!=1 {exit 1}
      {n=split($0,a,"."); if(kind=="macos") {if(n!=2 && n!=3) exit 1} else if(kind=="glibc") {if(n!=2 && n!=3) exit 1} else exit 1;
       for(i=1;i<=n;i++) if(a[i]!~/^(0|[1-9][0-9]*)$/ || length(a[i])>5) exit 1;
       if(a[1]+0<1) exit 1; if(kind=="macos" && n==2) {a[3]=0; n=3}
       result=a[1]; for(i=2;i<=n;i++) result=result"."a[i]; print result; ok=1}
      END {if(!ok || NR!=1) exit 1}'
}
inspect_platform() {
    if [ "$platform" = Darwin ]; then
        bounded "$stage/platform.out" 4194304 30 otool -hv -l "$1"
        awk '
          $1=="MH_MAGIC_64" {if($2!="ARM64" || $5!="EXECUTE") bad=1; headers++}
          $1=="cmd" {cmd=$2}
          cmd=="LC_BUILD_VERSION" && $1=="platform" {if($2!="1" && $2!="macos" && $2!="MACOS") bad=1; platforms++}
          cmd=="LC_BUILD_VERSION" && $1=="minos" {minimum=$2; minima++}
          cmd=="LC_VERSION_MIN_MACOSX" && $1=="version" {minimum=$2; minima++; legacy++}
          $1=="cmd" && $2~/^LC_VERSION_MIN_/ && $2!="LC_VERSION_MIN_MACOSX" {bad=1}
          END {if(bad || headers!=1 || minima!=1 || (!legacy && platforms!=1)) exit 1; print minimum}' "$stage/platform.out" > "$stage/platform.version" || fail 'cannot derive one ARM64 macOS deployment requirement'
    else
        bounded "$stage/platform.out" 4194304 30 readelf --file-header --version-info --wide "$1"
        awk '
          /^[ ]*Class:/ {if($2!="ELF64") bad=1; classes++}
          /^[ ]*Data:/ {if($0!~/little endian/) bad=1; data++}
          /^[ ]*Type:/ {if($2!="EXEC" && $2!="DYN") bad=1; types++}
          /^[ ]*Machine:/ {if($0!~/Advanced Micro Devices X86-64/) bad=1; machines++}
          /^Version needs section / {needs=1; next}
          /^Version / {needs=0}
          needs {for(i=1;i<NF;i++) if($i=="Name:" && $(i+1)~/^GLIBC_/) {
            tag=$(i+1); sub(/^GLIBC_/,"",tag); n=split(tag,a,".");
            if((n!=2 && n!=3) || a[1]+0<1) bad=1; for(j=1;j<=n;j++) if(a[j]!~/^(0|[1-9][0-9]*)$/ || length(a[j])>5) bad=1;
            if(!count++ || a[1]+0>major || a[1]+0==major && (a[2]+0>minor || a[2]+0==minor && a[3]+0>patch)) {major=a[1]+0; minor=a[2]+0; patch=a[3]+0; minimum=tag}}}
          END {if(bad || classes!=1 || data!=1 || types!=1 || machines!=1 || !count) exit 1; print minimum}' "$stage/platform.out" > "$stage/platform.version" || fail 'cannot derive numeric x86_64 GLIBC requirements; nonnumeric ABI nodes need separate support'
    fi
    observed=$(normalize_platform_version "$(cat "$stage/platform.version")" "$requirement_kind") || fail 'invalid executable minimum platform version'
    [ "$observed" = "$requirement_version" ] || fail "executable requires $requirement_kind $observed; release metadata declares $requirement_version"
}
check_host_requirement() {
    if [ "$requirement_kind" = macos ]; then
        bounded "$stage/platform.host" 1024 10 sw_vers -productVersion
        host_macos=$(awk 'NR==1 && NF==1 {print; ok=1} END {if(NR!=1 || !ok) exit 1}' "$stage/platform.host") || fail 'cannot determine current macOS version'
        host_version=$(normalize_platform_version "$host_macos" macos) || fail 'cannot determine current macOS version'
        label=macOS
    else
        bounded "$stage/platform.host" 1024 10 getconf GNU_LIBC_VERSION
        host_glibc=$(awk 'NR==1 && NF==2 && $1=="glibc" {print $2; ok=1} END {if(NR!=1 || !ok) exit 1}' "$stage/platform.host") || fail 'this package requires GNU glibc; musl and unknown C libraries are unsupported'
        host_version=$(normalize_platform_version "$host_glibc" glibc) || fail 'cannot determine current glibc version'
        label=glibc
    fi
    awk -v required="$requirement_version" -v current="$host_version" 'BEGIN {
      n=split(required,a,"."); m=split(current,b,"."); if(m>n) n=m;
      for(i=1;i<=n;i++) {if(b[i]+0>a[i]+0) exit 0; if(b[i]+0<a[i]+0) exit 1}}
    ' || fail "this executable requires $label $requirement_version or newer; your system has $host_version. Update your system or choose a release built for it. The active executable was not changed."
}

parse_manifest() {
    regular "$1" && [ "$(length "$1")" -le 1024 ] || fail 'invalid release manifest file'
    # A printable ASCII file with exact LF records. Re-serialization rejects NUL,
    # CRLF, missing final newline, unknown fields, duplicate records and junk.
    awk -v ver="$2" -v selected="$target" '
    function hex(x,n) {return length(x)==n && x !~ /[^0-9a-f]/}
    function valid_version(x,kind,   n,a,i) {n=split(x,a,"."); if(kind=="macos"?n!=3:(n!=2 && n!=3)) return 0;
      for(i=1;i<=n;i++) if(a[i]!~/^(0|[1-9][0-9]*)$/ || length(a[i])>5) return 0; return a[1]+0>0}
    NR==1 {if($0!="lwiki.release-manifest.v2") exit 1; next}
    NR==2 {if(NF!=2 || $1!="version" || $2!=ver || $0!="version "ver) exit 1; next}
    NR==3 {if(NF!=2 || $1!="commit" || !(hex($2,40)||hex($2,64)) || $0!="commit "$2) exit 1; commit=$2; next}
    NR==4 || NR==5 {
      wanted=NR==4?"aarch64-apple-darwin":"x86_64-unknown-linux-gnu";
      if(NF!=8 || $1!="asset" || $2!=wanted || !hex($3,64) || !hex($4,64) ||
         $5!~/^[1-9][0-9]*$/ || length($5)>9 || $5+0>134217728 ||
         $6!~/^[1-9][0-9]*$/ || length($6)>9 || $6+0>134217728 ||
         $7!=(wanted=="aarch64-apple-darwin"?"macos":"glibc") || !valid_version($8,$7) ||
         $0!="asset "$2" "$3" "$4" "$5" "$6" "$7" "$8) exit 1;
      if($2==selected) row=commit" "$3" "$4" "$5" "$6" "$7" "$8; next}
    {exit 1}
    END {if(NR!=5 || row=="") exit 1; print row}' "$1" > "$stage/rows" || fail 'invalid fixed release manifest'
    # awk can accept an unterminated last line; check exact bytes too.
    awk '{print}' "$1" > "$stage/headers"
    cmp -s "$1" "$stage/headers" || fail 'manifest must use exact LF records'
    IFS=' ' read -r commit archive_hash binary_hash binary_bytes archive_bytes requirement_kind requirement_version < "$stage/rows"
}
json_check() {
    bounded "$stage/json.utf8" 1048576 10 iconv -f UTF-8 -t UTF-8 "$1"
    bounded "$stage/json.out" 1024 10 awk -v kind="$2" -v expected_version="$version" -v expected_commit="$commit" -v expected_target="$target" -v expected_hash="$binary_hash" -v expected_kind="$requirement_kind" -v expected_minimum="$requirement_version" '
    function ws() {while(substr(text,p,1)~/[ \t\r\n]/ && p<=length(text)) p++}
    function die() {bad=1; exit 1}
    function string(   c,s) {
      if(substr(text,p++,1)!="\"") die(); s="";
      while(p<=length(text)) {c=substr(text,p++,1); if(c=="\"") return s;
        if(c=="\\") {c=substr(text,p++,1); if(c~/["\\\/bfnrt]/) s=s"\\"c;
          else if(c=="u" && substr(text,p,4)~/^[0-9a-fA-F][0-9a-fA-F][0-9a-fA-F][0-9a-fA-F]$/) {s=s"\\u"substr(text,p,4); p+=4} else die()}
        else {if(c==sprintf("%c",0) || c~/[\001-\037]/) die(); s=s c}}
      die()
    }
    function val(path,   c,k,n,token,start,mark) {
      ws(); c=substr(text,p,1);
      if(c=="{") {type[path]="object"; p++; ws(); if(substr(text,p,1)=="}") {p++; return}
        while(1) {ws(); k=string(); if(k~/\\/ || k~/[^ -~]/) die(); mark=path SEP k;
          if(seen[mark]++) die(); ws(); if(substr(text,p++,1)!=":") die(); val(mark); ws(); c=substr(text,p++,1);
          if(c=="}") return; if(c!=",") die()}}
      if(c=="[") {type[path]="array"; p++; ws(); n=0; if(substr(text,p,1)=="]") {p++; return}
        while(1) {val(path SEP n++); ws(); c=substr(text,p++,1); if(c=="]") {count[path]=n; return} if(c!=",") die()}}
      if(c=="\"") {type[path]="string"; value[path]=string(); return}
      start=p; while(p<=length(text) && substr(text,p,1)!~/[ \t\r\n,\]}]/) p++;
      token=substr(text,start,p-start);
      if(token=="true" || token=="false" || token=="null") {type[path]=token; value[path]=token; return}
      if(token !~ /^-?(0|[1-9][0-9]*)(\.[0-9]+)?([eE][+-]?[0-9]+)?$/) die(); type[path]="number"; value[path]=token
    }
    {text=text $0"\n"}
    END {if(bad) exit 1; SEP=sprintf("%c",28); p=1; val(""); ws(); if(p<=length(text) || type[""]!="object") die();
      if(kind=="caps") {if(type[SEP"ok"]!="true" || value[SEP"command"]!="capabilities") die()}
      else {if(value[SEP"schema_version"]!="2" || type[SEP"schema_version"]!="number" ||
        value[SEP"source"SEP"commit"]!=expected_commit || type[SEP"source"SEP"dirty"]!="false" ||
        value[SEP"target"]!=expected_target || value[SEP"host"]!=expected_target ||
        value[SEP"package_version"]!=expected_version || value[SEP"binary"]!="lwiki" || value[SEP"binary_sha256"]!=expected_hash || type[SEP"build_command"]!="array" ||
        type[SEP"platform_requirements"]!="object" ||
        value[SEP"platform_requirements"SEP"kind"]!=expected_kind ||
        value[SEP"platform_requirements"SEP"minimum_version"]!=expected_minimum) die();
        for(k in seen) if(index(k,SEP"platform_requirements"SEP)==1 && k!=SEP"platform_requirements"SEP"kind" && k!=SEP"platform_requirements"SEP"minimum_version") die();
        for(i=0;i<count[SEP"build_command"];i++) {s=value[SEP"build_command"SEP i]; if(s=="build") b=1; if(s=="--config=release") r=1} if(!b || !r) die()}}
    ' "$stage/json.utf8"
}
check_artifact() {
    awk -v ver="$version" -v sha="$commit" -v target="$target" -v kind="$requirement_kind" -v minimum="$requirement_version" '
      NR==1 {if($0!="lwiki.artifact-info.v2") exit 1; next}
      NR==2 {if($0!="version "ver) exit 1; next}
      NR==3 {if($0!="commit "sha) exit 1; next}
      NR==4 {if($0!="target "target) exit 1; next}
      NR==5 {if($0!="source_clean true") exit 1; next}
      NR==6 {if($0!="requirement "kind" "minimum) exit 1; next}
      NR>=7 && NR<=13 {split("lwiki version.txt capabilities.json build-info.json LICENSE THIRD-PARTY-NOTICES INSTALL.txt",names," ");
        if(NF!=4 || $1!="file" || $2!=names[NR-6] || length($3)!=64 || $3~/[^0-9a-f]/ ||
           $4!~/^[1-9][0-9]*$/ || length($4)>9 || $4+0>134217728 || $0!="file "$2" "$3" "$4) exit 1;
        print $2" "$3" "$4; next}
      {exit 1} END {if(NR!=13) exit 1}' "$stage/artifact-info.txt" > "$stage/rows" || fail 'invalid artifact identity metadata'
    awk '{print}' "$stage/artifact-info.txt" > "$stage/headers"
    cmp -s "$stage/artifact-info.txt" "$stage/headers" || fail 'artifact metadata must use exact LF records'
    while IFS=' ' read -r name digest bytes; do
        regular "$stage/$name" && [ "$(length "$stage/$name")" = "$bytes" ] && [ "$(hash "$stage/$name")" = "$digest" ] || fail "member identity mismatch: $name"
    done < "$stage/rows"
    [ "$(hash "$stage/lwiki")" = "$binary_hash" ] && [ "$(length "$stage/lwiki")" = "$binary_bytes" ] || fail 'manifest executable identity mismatch'
    [ "$(cat "$stage/version.txt"; printf '.')" = "lwiki $version
." ] || fail 'recorded executable version mismatch'
    json_check "$stage/build-info.json" info
    json_check "$stage/capabilities.json" caps
    inspect_platform "$stage/lwiki"
}
read_archive() {
    regular "$stage/archive.tar.gz" && [ "$(length "$stage/archive.tar.gz")" = "$archive_bytes" ] && [ "$(hash "$stage/archive.tar.gz")" = "$archive_hash" ] || fail 'archive identity mismatch'
    bounded "$stage/archive.tar" 201326592 30 gzip -dc "$stage/archive.tar.gz"
    tar_bytes=$(length "$stage/archive.tar")
    [ "$((tar_bytes % 512))" = 0 ] || fail 'unaligned decoded tar'
    block=0; count=0; names=' '
    : > "$stage/headers"
    while [ "$((block * 512 + 512))" -le "$tar_bytes" ]; do
        dd if="$stage/archive.tar" of="$stage/header" bs=512 skip="$block" count=1 2>/dev/null || fail 'cannot read tar header'
        od -An -v -tu1 "$stage/header" > "$stage/header.bytes"
        awk '
          function octal(start,n,   i,c,x,done,digits) {x=0; for(i=start;i<start+n;i++) {c=a[i];
            if(c==0 || c==32) done=1; else {if(done || c<48 || c>55) exit 1; digits++; x=x*8+c-48}} if(!digits) exit 1; return x}
          {for(i=1;i<=NF;i++) a[++n]=$i}
          END {if(n!=512) exit 1; sum=0; for(i=1;i<=512;i++) sum+=a[i]; if(sum==0) {print "END"; exit}
            if(a[157]!=48 || a[258]!=117 || a[259]!=115 || a[260]!=116 || a[261]!=97 || a[262]!=114 || a[263]!=0 || a[264]!=48 || a[265]!=48) exit 1;
            for(i=158;i<=257;i++) if(a[i]!=0) exit 1;
            for(i=346;i<=512;i++) if(a[i]!=0) exit 1;
            name=""; ended=0; for(i=1;i<=100;i++) {if(a[i]==0) ended=1; else {if(ended || a[i]<32 || a[i]>126) exit 1; name=name sprintf("%c",a[i])}} if(!ended) exit 1;
            if(name!~/^(lwiki|version\.txt|capabilities\.json|build-info\.json|LICENSE|THIRD-PARTY-NOTICES|INSTALL\.txt|artifact-info\.txt)$/) exit 1;
            actual=sum; for(i=149;i<=156;i++) actual+=32-a[i]; if(actual!=octal(149,8)) exit 1;
            mode=octal(101,8); size=octal(125,12); if(mode>511 || size<1) exit 1;
            if(name=="lwiki") {if(size>134217728 || (int(mode/64)%8)%2==0 && (int(mode/8)%8)%2==0 && (mode%8)%2==0) exit 1}
            else if(name=="THIRD-PARTY-NOTICES") {if(size>16777216) exit 1} else if(size>1048576) exit 1;
            print name" "size}' "$stage/header.bytes" > "$stage/rows" || fail 'unsupported or invalid physical USTAR header'
        IFS=' ' read -r name size < "$stage/rows"
        if [ "$name" = END ]; then
            [ "$((block * 512 + 1024))" -le "$tar_bytes" ] || fail 'missing second tar terminator'
            # The entire tail must be zero, including any concatenated archive.
            [ "$((tar_bytes - block * 512))" -le 10240 ] || fail 'oversized tar terminator padding'
            dd if="$stage/archive.tar" of="$stage/header" bs=512 skip="$block" 2>/dev/null || fail 'cannot read tar tail'
            od -An -v -tu1 "$stage/header" > "$stage/header.bytes" || fail 'cannot decode tar tail'
            awk '{for(i=1;i<=NF;i++) if($i!=0) exit 1}' "$stage/header.bytes" || fail 'nonzero trailing archive bytes'
            break
        fi
        case "$names" in *" $name "*) fail 'duplicate archive member' ;; esac
        names="$names$name "; count=$((count + 1))
        [ "$count" -le 8 ] || fail 'extra archive member'
        end=$((block * 512 + 512 + ((size + 511) / 512) * 512))
        [ "$end" -le "$tar_bytes" ] || fail 'truncated archive member'
        padding=$(( (512 - size % 512) % 512 ))
        if [ "$padding" -gt 0 ]; then
            dd if="$stage/archive.tar" of="$stage/header" bs=1 skip="$((block * 512 + 512 + size))" count="$padding" 2>/dev/null || fail 'cannot read member padding'
            od -An -v -tu1 "$stage/header" > "$stage/header.bytes" || fail 'cannot decode member padding'
            awk '{for(i=1;i<=NF;i++) if($i!=0) exit 1}' "$stage/header.bytes" || fail 'nonzero member padding'
        fi
        printf '%s %s\n' "$name" "$size" >> "$stage/headers"
        block=$((end / 512))
    done
    [ "$name" = END ] && [ "$count" = 8 ] || fail 'archive must contain eight regular root files'
    while IFS=' ' read -r name size; do
        bounded "$stage/$name" "$size" 10 tar -xOf "$stage/archive.tar" "$name"
        [ "$(length "$stage/$name")" = "$size" ] || fail "extracted length mismatch: $name"
    done < "$stage/headers"
    check_artifact
}
verify_generation() {
    generation=$1
    check_dir "$generation" prefix
    base=${generation##*/}
    case "$base" in *"-$target-"*) ;; *) fail 'retained generation target mismatch' ;; esac
    identity=${base##*-}
    printf '%s\n' "$identity" | awk 'length($0)==64 && $0!~/[^0-9a-f]/ {ok=1} END {exit !ok}' || fail 'invalid retained generation identity'
    regular "$generation/receipt" && [ "$(length "$generation/receipt")" -le 4096 ] && [ "$(hash "$generation/receipt")" = "$identity" ] || fail 'retained receipt changed or missing'
    for entry in "$generation"/* "$generation"/.[!.]* "$generation"/..?*; do
        [ -e "$entry" ] || [ -L "$entry" ] || continue
        case "${entry##*/}" in lwiki|version.txt|capabilities.json|build-info.json|LICENSE|THIRD-PARTY-NOTICES|INSTALL.txt|artifact-info.txt|release-manifest.txt|receipt) regular "$entry" || fail 'nonregular retained member' ;; *) fail 'unknown retained generation file' ;; esac
    done
    awk 'NR==1 {if($0!="lwiki.install-receipt.v1") exit 1; next}
      NR>=2 && NR<=10 {split("lwiki version.txt capabilities.json build-info.json LICENSE THIRD-PARTY-NOTICES INSTALL.txt artifact-info.txt release-manifest.txt",names," ");
        if(NF!=3 || $1!=names[NR-1] || length($2)!=64 || $2~/[^0-9a-f]/ || $3!~/^[1-9][0-9]*$/ || length($3)>9 || $3+0>134217728 || $0!=$1" "$2" "$3) exit 1; print; next}
      {exit 1} END {if(NR!=10) exit 1}' "$generation/receipt" > "$stage/rows" || fail 'invalid retained receipt'
    awk '{print}' "$generation/receipt" > "$stage/headers"
    cmp -s "$generation/receipt" "$stage/headers" || fail 'receipt must use exact LF records'
    while IFS=' ' read -r name digest bytes; do
        regular "$generation/$name" && [ "$(length "$generation/$name")" = "$bytes" ] && [ "$(hash "$generation/$name")" = "$digest" ] || fail "retained file changed: $name"
    done < "$stage/rows"
    [ "$(stat_fields "$generation/lwiki")" = "$uid 755" ] || fail 'retained executable owner or mode changed'
    retained_version=${base%"-$target-$identity"}
    printf '%s\n' "$retained_version" | awk 'length($0)<=80 && /^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?(\+[0-9A-Za-z.-]+)?$/ {ok=1} END {exit !ok}' || fail 'invalid retained version'
    parse_manifest "$generation/release-manifest.txt" "$retained_version"
    [ "$base" = "$retained_version-$target-$identity" ] || fail 'retained directory identity mismatch'
    # Validate semantic metadata again using private fixed copies.
    for name in lwiki version.txt capabilities.json build-info.json LICENSE THIRD-PARTY-NOTICES INSTALL.txt artifact-info.txt; do cp "$generation/$name" "$stage/$name"; done
    saved_version=$version; version=$retained_version
    check_artifact
    version=$saved_version
}
check_activation() {
    check_managed
    if [ -e "$managed/current" ] || [ -L "$managed/current" ]; then
        [ -L "$managed/current" ] || fail 'current activation is not a symlink'
        current=$(readlink "$managed/current"; printf '.')
        case "$current" in 'versions/'*'
.') current=${current%'
.'} ;; *) fail 'invalid current activation link' ;; esac
        relative=${current#versions/}
        case "$relative" in */*|*'
'*|'') fail 'invalid current generation path' ;; esac
        verify_generation "$managed/$current"
    elif [ -e "$launch" ] || [ -L "$launch" ]; then fail 'launch exists without activation'; fi
    if [ -e "$launch" ] || [ -L "$launch" ]; then
        [ -L "$launch" ] && [ "$(readlink "$launch"; printf '.')" = '../.lwiki-install/current/lwiki
.' ] || fail 'launch link changed'
    fi
}
# All retained versions must have intact receipts. This prevents a version-looking
# directory from becoming authority merely because it was present in the prefix.
if [ -e "$managed" ]; then
    for generation in "$managed/versions"/* "$managed/versions"/.[!.]* "$managed/versions"/..?*; do
        [ -e "$generation" ] || [ -L "$generation" ] || continue
        verify_generation "$generation"
    done
    check_activation
fi
if [ "$mode" = uninstall ]; then
    printf 'Uninstall target=%s prefix=%s retained_versions=yes dry_run=%s\n' "$target" "$prefix" "$dry"
    if [ "$dry" = false ]; then
        check_activation
        [ ! -L "$launch" ] || rm "$launch"
        [ ! -L "$managed/current" ] || rm "$managed/current"
    fi
    exit 0
fi
if [ "$mode" = rollback ]; then
    selected=
    for generation in "$managed/versions/$version-$target-"*; do
        [ -e "$generation" ] || [ -L "$generation" ] || continue
        [ -z "$selected" ] || fail 'more than one retained generation for this version'
        verify_generation "$generation"; selected=$generation
    done
    [ -n "$selected" ] || fail 'requested version is not retained'
    check_host_requirement
else
    if [ -n "$from_dir" ]; then
        regular "$from_dir/release-manifest.txt" && [ "$(length "$from_dir/release-manifest.txt")" -le 1024 ] || fail 'local manifest is invalid or oversized'
        cp "$from_dir/release-manifest.txt" "$stage/release-manifest.txt"
    else
        curl_version=$(curl -q --version | awk 'NR==1 {print $2}')
        printf '%s\n' "$curl_version" | awk -F. '$1+0>8 || $1==8 && $2+0>=4 {ok=1} END {exit !ok}' || fail 'online installation requires curl 8.4 or newer'
        curl -q --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --connect-timeout 15 --max-time 90 --max-filesize 1024 "https://github.com/joshgarnett/rust-llm-wiki/releases/download/v$version/release-manifest.txt" -o "$stage/release-manifest.txt" || fail 'manifest download failed'
    fi
    parse_manifest "$stage/release-manifest.txt" "$version"
    archive_name=lwiki-$version-$target.tar.gz
    if [ -n "$from_dir" ]; then
        regular "$from_dir/$archive_name" && [ "$(length "$from_dir/$archive_name")" -le 134217728 ] || fail 'local archive is invalid or oversized'
        cp "$from_dir/$archive_name" "$stage/archive.tar.gz"
        if [ "$platform" = Darwin ]; then
            xattr "$from_dir/$archive_name" > "$stage/quarantine" || fail 'cannot inspect archive quarantine attributes'
            if awk '$0=="com.apple.quarantine" {found=1} END {exit !found}' "$stage/quarantine"; then
                xattr -p com.apple.quarantine "$from_dir/$archive_name" > "$stage/quarantine" || fail 'cannot read archive quarantine'
                [ "$(length "$stage/quarantine")" -le 8192 ] || fail 'oversized quarantine attribute'
                xattr -w com.apple.quarantine "$(cat "$stage/quarantine")" "$stage/archive.tar.gz" || fail 'cannot preserve archive quarantine'
            else : > "$stage/quarantine"; fi
        fi
    else
        curl -q --fail --silent --show-error --location --proto '=https' --proto-redir '=https' --connect-timeout 15 --max-time 90 --max-filesize 134217728 "https://github.com/joshgarnett/rust-llm-wiki/releases/download/v$version/$archive_name" -o "$stage/archive.tar.gz" || fail 'archive download failed'
    fi
    read_archive
    check_host_requirement
    chmod 755 "$stage/lwiki"
    if [ "$platform" = Darwin ] && regular "$stage/quarantine" && [ -s "$stage/quarantine" ]; then
        xattr -w com.apple.quarantine "$(cat "$stage/quarantine")" "$stage/lwiki" || fail 'cannot preserve executable quarantine'
    fi
    printf 'lwiki.install-receipt.v1\n' > "$stage/receipt"
    for name in lwiki version.txt capabilities.json build-info.json LICENSE THIRD-PARTY-NOTICES INSTALL.txt artifact-info.txt release-manifest.txt; do
        printf '%s %s %s\n' "$name" "$(hash "$stage/$name")" "$(length "$stage/$name")" >> "$stage/receipt"
    done
    gen_name=$version-$target-$(hash "$stage/receipt")
    selected=$managed/versions/$gen_name
    if [ -e "$managed" ]; then
        for generation in "$managed/versions/$version-$target-"*; do
            [ -e "$generation" ] || [ -L "$generation" ] || continue
            [ "$generation" = "$selected" ] || fail 'this version already exists with different bytes'
            verify_generation "$generation"
        done
    fi
    if [ "$dry" = false ]; then
        bounded "$stage/version.out" 1024 10 "$stage/lwiki" --version
        cmp -s "$stage/version.txt" "$stage/version.out" || fail 'staged executable returned an unexpected version'
    fi
    if [ "$dry" = false ] && [ ! -e "$selected" ]; then
        # A complete immutable generation is published before any launch changes.
        mkdir "$stage/generation"
        for name in lwiki version.txt capabilities.json build-info.json LICENSE THIRD-PARTY-NOTICES INSTALL.txt artifact-info.txt release-manifest.txt receipt; do
            mv "$stage/$name" "$stage/generation/$name"
        done
        check_managed
        mv "$stage/generation" "$selected"
        verify_generation "$selected"
    fi
fi
printf 'Install target=%s version=%s commit=%s prefix=%s dry_run=%s\n' "$target" "$version" "$commit" "$prefix" "$dry"
[ "$dry" = false ] || exit 0
selected_requirement_kind=$requirement_kind; selected_requirement_version=$requirement_version
check_activation
requirement_kind=$selected_requirement_kind; requirement_version=$selected_requirement_version
check_host_requirement
mkdir -p "$prefix/bin"
check_dir "$prefix/bin" prefix
ln -s "versions/${selected##*/}" "$stage/next-current"
check_activation
replace_link "$stage/next-current" "$managed/current"
[ "$(readlink "$managed/current"; printf '.')" = "versions/${selected##*/}
." ] || fail 'activation did not replace the current link'
if [ ! -e "$launch" ] && [ ! -L "$launch" ]; then
    ln -s '../.lwiki-install/current/lwiki' "$stage/next-launch"
    check_activation
    check_dir "$prefix/bin" prefix
    replace_link "$stage/next-launch" "$launch"
fi
check_activation
printf 'Installed %s. Add %s/bin to PATH if needed.\n' "$version" "$prefix"
