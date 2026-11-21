package payments

import (
	"encoding/json"
	"os"
	"testing"
)

func bumpAttempt(t *testing.T) int {
	t.Helper()
	path := os.Getenv("FASTPROOF_COUNTER_FILE")
	if path == "" {
		t.Fatal("FASTPROOF_COUNTER_FILE is not set")
	}
	counts := map[string]int{}
	if data, err := os.ReadFile(path); err == nil {
		_ = json.Unmarshal(data, &counts)
	}
	counts[t.Name()]++
	data, _ := json.Marshal(counts)
	if err := os.WriteFile(path, data, 0o644); err != nil {
		t.Fatal(err)
	}
	return counts[t.Name()]
}

func TestPaymentGatewayFlaky(t *testing.T) {
	attempt := bumpAttempt(t)
	t.Logf("attempt %d", attempt)
	if attempt < 2 {
		t.Errorf("gateway timeout on attempt %d", attempt)
	}
}

func TestRefundAlwaysFails(t *testing.T) {
	attempt := bumpAttempt(t)
	t.Fatalf("refund rejected on attempt %d: got status %q, want %q", attempt, "DECLINED", "APPROVED")
}

func TestCurrencyRounding(t *testing.T) {
	if got, want := int(2.675*100+0.5), 268; got != want {
		t.Errorf("rounded = %d, want %d", got, want)
	}
}
