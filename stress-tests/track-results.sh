#!/bin/bash
# Track stress test results with git history and rolling averages

set -e

# Get the directory where this script is located
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
cd "$SCRIPT_DIR/.."

RESULTS_FILE="stress-test-results.md"
GIT_LOG_PATTERN="%h %s"

# Get current commit hash and timestamp
CURRENT_COMMIT=$(git rev-parse --short HEAD)
TIMESTAMP=$(date -u +"%Y-%m-%d %H:%M:%S UTC")

# Get commit messages since last stress test
LAST_COMMIT=""
if [ -f "$RESULTS_FILE" ]; then
    # Extract the last commit hash from the results file
    LAST_COMMIT=$(grep -m 1 "^## Commit:" "$RESULTS_FILE" | cut -d' ' -f3 || echo "")
fi

if [ -z "$LAST_COMMIT" ]; then
    # First run - get all commits
    COMMIT_LOG=$(git log --pretty=format:"$GIT_LOG_PATTERN" -20)
else
    # Get commits since last stress test
    COMMIT_LOG=$(git log --pretty=format:"$GIT_LOG_PATTERN" ${LAST_COMMIT}..HEAD 2>/dev/null || echo "No commits since last test")
fi

# Run the stress tests and capture output
echo "Running stress tests..."
TEST_OUTPUT=$(cargo run --release --package duroxide-stress-tests --bin parallel_orchestrations 2>&1)

# Extract comparison table from the output, stripping ANSI escape codes
# Find the table section and extract lines starting with "INFO" that contain the table
RESULTS=$(echo "$TEST_OUTPUT" | awk '/=== Comparison Table ===/{found=1} found && /INFO.*duroxide_stress_tests:/{print}' | sed 's/.*INFO duroxide_stress_tests: //' | sed 's/\x1b\[[0-9;]*m//g')

# Create the results entry
ENTRY_FILE=$(mktemp)
cat > "$ENTRY_FILE" << EOF

---

## Commit: $CURRENT_COMMIT - Timestamp: $TIMESTAMP

### Changes Since Last Test
\`\`\`
$COMMIT_LOG
\`\`\`

### Test Results
\`\`\`
$RESULTS
\`\`\`

EOF

# Prepend to results file
if [ -f "$RESULTS_FILE" ]; then
    # Read existing content, prepend new entry, and write back
    # Extract header if present, otherwise use default
    if grep -q "^# Duroxide Stress Test Results" "$RESULTS_FILE"; then
        # Keep header at top
        HEADER_LINES=$(grep -n "^# Duroxide Stress Test Results" "$RESULTS_FILE" | cut -d: -f1)
        HEADER_LINES=$((HEADER_LINES + 3))  # Include header and 2 blank lines after
        HEADER=$(head -n $HEADER_LINES "$RESULTS_FILE")
        CONTENT=$(tail -n +$((HEADER_LINES + 1)) "$RESULTS_FILE")
        (echo "$HEADER" && cat "$ENTRY_FILE" && echo "$CONTENT") > temp_results.md
    else
        # No header found, prepend entry
        (cat "$ENTRY_FILE" && cat "$RESULTS_FILE") > temp_results.md
    fi
    mv temp_results.md "$RESULTS_FILE"
else
    # Create new file with header
    cat > "$RESULTS_FILE" << 'EOF'
# Duroxide Stress Test Results

This file tracks all stress test runs, including performance metrics and commit changes.

EOF
    cat "$ENTRY_FILE" >> "$RESULTS_FILE"
fi

rm "$ENTRY_FILE"

echo ""
echo "=========================================="
echo "Stress Test Results Tracked"
echo "=========================================="
echo "Commit: $CURRENT_COMMIT"
echo "Results saved to: $RESULTS_FILE"
echo ""
echo "Changes since last test:"
echo "$COMMIT_LOG"
echo ""
echo "Test Results:"
echo "$RESULTS"
