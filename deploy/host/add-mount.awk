# Adds one volume line to a compose file somebody wrote themselves, under services.<svc>, and
# leaves every other line as it was. Both lines it may add end in "# uwumail-host", so
# install.sh --remove can take exactly those back out.
#
#   awk -v svc=uwumail -v entry=/var/lib/uwumail-host/bridge:/host -f add-mount.awk file file
#
# The file is read twice: the first pass finds where things are, the second prints. Exits 1 and
# prints nothing when it cannot tell where the line goes (no such service, or a flow-style
# `uwumail: {...}`); install.sh then says what to add instead. Whatever it prints is checked with
# docker compose config before it replaces anything.
function indent(line) { match(line, /^ */); return RLENGTH }
function content(line) { return line !~ /^[ \t]*(#.*)?$/ }

FNR == 1 { pass++ }

pass == 1 {
  if ($0 ~ /^services:[ \t]*(#.*)?$/) { in_services = 1; next }
  if (in_services && content($0) && indent($0) == 0) in_services = 0
  if (!svc_line && in_services && $0 ~ ("^ +" svc ":[ \t]*(#.*)?$")) {
    svc_line = FNR; svc_indent = indent($0); next
  }
  if (svc_line && !svc_done && content($0)) {
    if (indent($0) <= svc_indent) { svc_done = 1; next }
    if (!child) child = indent($0)
    if (indent($0) == child && $0 ~ /^ *volumes:[ \t]*(#.*)?$/) { vol_line = FNR; next }
    if (vol_line && FNR > vol_line && (indent($0) < child || (indent($0) == child && $0 !~ /^ *- /))) vol_closed = 1
    if (vol_line && !vol_closed && !item && $0 ~ /^ *- /) item = indent($0)
  }
  next
}

pass == 2 && FNR == 1 {
  if (!svc_line) exit 1
  if (!child) child = svc_indent + 2
  if (!item) item = child + 2
  pad_child = sprintf("%" child "s", ""); pad_item = sprintf("%" item "s", "")
}

{ print }

FNR == vol_line { print pad_item "- " entry "  # uwumail-host" }
!vol_line && FNR == svc_line {
  print pad_child "volumes:  # uwumail-host"
  print pad_child "  - " entry "  # uwumail-host"
}
