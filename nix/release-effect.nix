# Publishes VERSION as a GitHub release when no release of that version
# exists yet, so every push to main is a no-op until VERSION changes.
# release-notes/v<VERSION>.md is the body; without one, GitHub generates it.
{ pkgs, hci-effects, version, release, notes, rev, repo }:
hci-effects.mkEffect {
  name = "release";
  inputs = [ pkgs.gh ];
  secretsMap.github = { type = "GitToken"; };
  effectScript = ''
    export GH_TOKEN=$(readSecretString github .token)
    if gh release view v${version} --repo ${repo} > /dev/null 2>&1; then
      echo "v${version} is already released"
      exit 0
    fi
    gh release create v${version} --repo ${repo} --target ${rev} \
      --title "iets v${version}" \
      ${if notes == null then "--generate-notes" else "--notes-file ${notes}"} \
      ${release}/*
  '';
}
