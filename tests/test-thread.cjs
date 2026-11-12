// Live integration tests use an explicitly selected, disposable demo chat.
if (!process.env.GOAL_LAYERS_TEST_THREAD) throw new Error('Set GOAL_LAYERS_TEST_THREAD to a disposable demo chat ID');
module.exports = process.env.GOAL_LAYERS_TEST_THREAD;
