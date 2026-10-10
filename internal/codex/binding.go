package codex

import (
	"database/sql"
	"errors"
	"fmt"

	"github.com/Hogeexxl/Usagi/internal/source"
	"github.com/Hogeexxl/Usagi/internal/storage"
)

type BindingOutcome uint8

const (
	BindingBoundNow BindingOutcome = iota + 1
	BindingReady
	BindingSourceChanged
)

func bindOrValidate(tx *source.WriteTx, fingerprint string) (BindingOutcome, error) {
	var outcome BindingOutcome
	statusChanged := false
	err := tx.Private(func(privateTx storage.PrivateTx) error {
		var storedFingerprint sql.NullString
		var status string
		if err := privateTx.QueryRow(
			"SELECT home_fingerprint,binding_status FROM codex_adapter_state WHERE id=1",
		).Scan(&storedFingerprint, &status); err != nil {
			return err
		}
		switch {
		case status == "unbound" && !storedFingerprint.Valid:
			result, err := privateTx.Exec(
				"UPDATE codex_adapter_state SET home_fingerprint=?,binding_status='ready' WHERE id=1 AND binding_status='unbound' AND home_fingerprint IS NULL",
				fingerprint,
			)
			if err != nil {
				return err
			}
			if err := requireOneRow(result); err != nil {
				return err
			}
			outcome = BindingBoundNow
			return nil
		case status == "ready" && storedFingerprint.Valid && storedFingerprint.String == fingerprint:
			outcome = BindingReady
			return nil
		case status == "ready" && storedFingerprint.Valid && storedFingerprint.String != fingerprint:
			result, err := privateTx.Exec(
				"UPDATE codex_adapter_state SET binding_status='source_changed' WHERE id=1 AND binding_status='ready' AND home_fingerprint=?",
				storedFingerprint.String,
			)
			if err != nil {
				return err
			}
			if err := requireOneRow(result); err != nil {
				return err
			}
			outcome = BindingSourceChanged
			statusChanged = true
			return nil
		case status == "source_changed" && storedFingerprint.Valid:
			outcome = BindingSourceChanged
			return nil
		default:
			return fmt.Errorf("invalid Codex binding state: status=%q fingerprint_present=%t", status, storedFingerprint.Valid)
		}
	})
	if err != nil {
		return 0, err
	}
	if outcome == BindingBoundNow || statusChanged {
		if _, err := tx.BumpStatusRevision(); err != nil {
			return 0, err
		}
	}
	return outcome, nil
}

func requireOneRow(result sql.Result) error {
	rows, err := result.RowsAffected()
	if err != nil {
		return err
	}
	if rows != 1 {
		return errors.New("Codex binding compare-and-swap failed")
	}
	return nil
}
