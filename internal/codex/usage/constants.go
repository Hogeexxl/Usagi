package usage

const UsageParserVersion int64 = 12
const UsageCanonicalAlgorithmVersion int64 = 6
const CompactionVisibilityReadyParserVersion int64 = 12

func CanonicalAlgorithmFor(parser int64) (int64, bool) {
	switch parser {
	case 4, 5:
		return 4, true
	case 6, 7, 8, 9, 10, 11:
		return 5, true
	case UsageParserVersion:
		return UsageCanonicalAlgorithmVersion, true
	default:
		return 0, false
	}
}
