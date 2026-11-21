package checkout

import "testing"

func total(prices []int, discountPercent int) int {
	sum := 0
	for _, p := range prices {
		sum += p
	}
	return sum
}

func TestCheckoutPasses(t *testing.T) {
	t.Log("checkout started")
	if got, want := total([]int{10, 10}, 0), 20; got != want {
		t.Errorf("total() = %d, want %d", got, want)
	}
}

func TestDiscountApplied(t *testing.T) {
	got := total([]int{10, 10}, 10)
	want := 18
	if got != want {
		t.Errorf("total with discount WELCOME10: got %d, want %d", got, want)
	}
}

func TestGiftCardsSkipped(t *testing.T) {
	t.Skip("gift cards are not enabled in this environment")
}

func TestCardValidation(t *testing.T) {
	cases := []struct {
		brand  string
		number string
		valid  bool
	}{
		{"visa", "4111111111111111", true},
		{"mastercard", "5500000000000004", true},
		{"unknown", "0000", true},
	}
	for _, tc := range cases {
		t.Run(tc.brand, func(t *testing.T) {
			if got := len(tc.number) == 16; got != tc.valid {
				t.Errorf("valid(%q) = %v, want %v", tc.number, got, tc.valid)
			}
		})
	}
}

func TestShipping(t *testing.T) {
	t.Run("domestic", func(t *testing.T) {
		t.Run("standard", func(t *testing.T) {})
		t.Run("express", func(t *testing.T) {
			t.Skip("express shipping disabled")
		})
	})
	t.Run("international", func(t *testing.T) {})
}
