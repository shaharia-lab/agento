package claudesessions

import "math"

// AutonomyScoreProcessor computes a 0–100 score that reflects how much the
// user let Claude run autonomously between interventions.
//
// Formula (applied in Finalize, after TurnCountProcessor has populated the insight):
//
//	if turnCount <= 1:
//	    score = 100 × min(1.0, log(stepsPerTurn+1) / log(10))
//	else:
//	    score = 100 × (1/turnCount) × min(1.0, log(stepsPerTurn+1) / log(10))
//	score = clamp(score, 0, 100)
//
// Higher score = fewer user interruptions relative to autonomous work done.
type AutonomyScoreProcessor struct{}

// Name returns the processor identifier.
func (p *AutonomyScoreProcessor) Name() string { return "autonomy_score" }

// Process is a no-op; this processor derives everything from TurnCountProcessor output.
func (p *AutonomyScoreProcessor) Process(_ ProcessableEvent) {}

// Finalize reads TurnCount and StepsPerTurnAvg from the insight (already
// populated by TurnCountProcessor) and writes AutonomyScore.
func (p *AutonomyScoreProcessor) Finalize(insight *SessionInsight) {
	turnCount := insight.TurnCount
	stepsPerTurn := insight.StepsPerTurnAvg

	var score float64
	logFactor := math.Min(1.0, math.Log(stepsPerTurn+1)/math.Log(10))

	if turnCount <= 1 {
		score = 100 * logFactor
	} else {
		score = 100 * (1.0 / float64(turnCount)) * logFactor
	}

	// Clamp to [0, 100].
	if score < 0 {
		score = 0
	}
	if score > 100 {
		score = 100
	}

	insight.AutonomyScore = score
}

// Reset is a no-op; this processor has no internal state.
func (p *AutonomyScoreProcessor) Reset() {}
