import { ClientSDK, RequestOptions } from "../lib/sdks.js";
import { MetricsLimits } from "../models/components/metricslimits.js";
import { MetricsResponse } from "../models/components/metricsresponse.js";
import { MetricsGetRequest } from "../models/operations/metricsget.js";
export declare class Metrics extends ClientSDK {
    /**
     * Get Metrics
     *
     * @remarks
     * Get metrics about your orders and subscriptions.
     *
     * Currency values are output in cents.
     *
     * **Scopes**: `metrics:read`
     */
    get(request: MetricsGetRequest, options?: RequestOptions): Promise<MetricsResponse>;
    /**
     * Get Metrics Limits
     *
     * @remarks
     * Get the interval limits for the metrics endpoint.
     *
     * **Scopes**: `metrics:read`
     */
    limits(options?: RequestOptions): Promise<MetricsLimits>;
}
//# sourceMappingURL=metrics.d.ts.map