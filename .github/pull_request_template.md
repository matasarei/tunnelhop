## What and why

<!-- What this changes, and the problem it solves. Link the issue if there is one. -->

## Testing

<!-- What you ran, and what it showed. "Not tested" is a fine answer; a missing one isn't. -->

## Checklist

- [ ] The pre-push hook passed (`git config core.hooksPath .githooks`)
- [ ] Tests cover the change, or it needs none (say why above)
- [ ] No real VPN configs, keys or credentials anywhere in the diff
- [ ] Kill-switch and live tunnel tests ran in a VM only, or none were needed
- [ ] `docs/SPEC.md` is updated if behaviour changed
- [ ] Third-party notices regenerated if a dependency changed
- [ ] No code copied from other VPN clients; anything taken from elsewhere is named above with
      its licence
