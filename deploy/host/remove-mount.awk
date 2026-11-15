# Takes out what add-mount.awk put into somebody's compose file: the marked list item, and the
# marked `volumes:` key unless they have since put items of their own under it -- then the key
# stays, without the mark.
#
#   awk -f remove-mount.awk file
{ line[NR] = $0 }
END {
  for (i = 1; i <= NR; i++) {
    if (line[i] ~ /^[ \t]*- .*  # uwumail-host$/) continue
    if (line[i] ~ /^[ \t]*volumes:  # uwumail-host$/) {
      j = i + 1
      while (j <= NR && (line[j] ~ /^[ \t]*(#.*)?$/ || line[j] ~ /^[ \t]*- .*  # uwumail-host$/)) j++
      if (!(j <= NR && line[j] ~ /^[ \t]*- /)) continue
      sub(/  # uwumail-host$/, "", line[i])
    }
    print line[i]
  }
}
