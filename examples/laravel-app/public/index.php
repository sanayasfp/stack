<?php

header('Content-Type: application/json');
echo json_encode([
    'message' => 'hello from laravel-app (skeleton), routed through stack',
    'note' => 'run `composer create-project laravel/laravel .` here for the real app',
]);
