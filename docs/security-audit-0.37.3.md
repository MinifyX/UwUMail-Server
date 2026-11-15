# Security audit — 0.37.3, updates page

A pass over what 0.37.3 changes (`git diff main...release/0.37.3`): the updates page in the portal
(`web/src/features/updates/UpdatesPage.tsx`) shows the restart buttons only when a restart is
needed and the note about other services in grey.

Done with Claude, not an independent firm; a reading of the diff.

## Summary

Nothing new. Display only: the restart steps the API accepts, their admin check and the confirm
dialog are unchanged; a restart that isn't needed is no longer offered in the page, the API still
takes it.
