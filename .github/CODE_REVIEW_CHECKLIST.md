# Code Review Checklist

Use this checklist when reviewing pull requests or your own code changes.

## Code Quality
- [ ] Code follows project style guidelines
- [ ] No unnecessary comments (code is self-documenting where possible)
- [ ] No hardcoded values that should be in config
- [ ] Error handling is appropriate
- [ ] No memory leaks (C code)
- [ ] No security vulnerabilities

## Functionality
- [ ] Feature works as described
- [ ] Edge cases are handled
- [ ] No regression in existing features
- [ ] Performance is acceptable

## Documentation Updates ⚠️ CRITICAL
**Every code change requires documentation updates!**

### Check if these files need updates:
- [ ] **README.md** - Main docs (features, usage, examples)
- [ ] **QUICKSTART.md** - Quick start guide
- [ ] **docs/PROTOCOL.md** - If protocol changed
- [ ] **docs/BUILDING.md** - If build process changed
- [ ] **IMPLEMENTATION_STATUS.md** - If features added/completed
- [ ] **Code comments** - Updated for clarity

### Verify Documentation Accuracy:
- [ ] All example commands are tested and work
- [ ] Screenshots/diagrams match current version
- [ ] Configuration examples are current
- [ ] File paths in docs are correct
- [ ] No broken links

## Testing
- [ ] Unit tests pass (if applicable)
- [ ] Integration tests pass
- [ ] Tested on real PS5 hardware (or equivalent)
- [ ] Tested edge cases
- [ ] Performance tested with large files

## Git Hygiene
- [ ] Commit messages are clear and descriptive
- [ ] No merge conflicts
- [ ] Branch is up to date with main
- [ ] No unnecessary files committed
- [ ] .gitignore is properly configured

## Before Merging
- [ ] All checklist items above are complete
- [ ] Documentation is 100% in sync with code
- [ ] CI/CD passes (if configured)
- [ ] At least one other person reviewed (if team project)

---

**Remember: Stale documentation is worse than no documentation. Always update docs with code changes!**
