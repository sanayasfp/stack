<?php

// This is a placeholder, not Laravel's real front controller -- see
// README.md. A real Laravel install generates this file itself (it boots
// the framework kernel via bootstrap/app.php); this stub exists only so
// `stack up` has something real to serve while you evaluate the manifest.

header('Content-Type: application/json');
echo json_encode([
    'message' => 'hello from laravel-app (skeleton), routed through stack',
    'note' => 'run `composer create-project laravel/laravel .` here for the real app',
]);
