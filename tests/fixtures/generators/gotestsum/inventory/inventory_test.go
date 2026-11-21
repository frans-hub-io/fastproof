package inventory

import "testing"

type store struct{ items map[string]int }

func TestStockLevel(t *testing.T) {
	var s *store
	if s.items["A-1"] < 0 {
		t.Error("negative stock")
	}
}
