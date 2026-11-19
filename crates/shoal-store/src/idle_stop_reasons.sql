-- Idle agents sleep for one reason now, the user being away from them, so
-- the stops recorded for the reasons it replaced take it.
UPDATE agent_stops SET reason='idle while the user is away'
    WHERE reason IN ('idle in a done workspace whose cleanup is held',
        'idle with its prompt cache expired', 'shoal sleep');
