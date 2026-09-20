import { tmpdir } from 'os';

/** Working directory for every process the server starts. Children otherwise
 *  inherit cwd=app\, and one that outlives the server blocks the update's rename. */
export const SPAWN_CWD = tmpdir();
