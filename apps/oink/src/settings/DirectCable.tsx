import { useEffect, useState } from 'react';
import { tr } from '../i18n';
import { api } from '../api/client';
import type { DirectCableStatus } from '../api/types';

/** Live cable state for the Direct connection section; polled while shown. */
export function DirectCableIndicator() {
    const [status, setStatus] = useState<DirectCableStatus | null>(null);
    useEffect(() => {
        let alive = true;
        const refresh = () => {
            api.directCableStatus().then(s => { if (alive) setStatus(s); }).catch(() => { if (alive) setStatus(null); });
        };
        refresh();
        const timer = window.setInterval(refresh, 2000);
        return () => { alive = false; window.clearInterval(timer); };
    }, []);
    if (!status) return <span>{tr('不可用')}</span>;
    const state = status.connected ? tr('已连接（{0}）', status.addresses.join(', ')) : tr('未连接');
    return <span role="status" aria-live="polite">
        {state}
        {status.paused_by_cable && <> · {tr('已因网线拔出暂停共享')}</>}
    </span>;
}
