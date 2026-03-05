import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { MetricsLimits } from "../models/components/metricslimits.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Get Metrics Limits
 *
 * @remarks
 * Get the interval limits for the metrics endpoint.
 *
 * **Scopes**: `metrics:read`
 */
export declare function metricsLimits(client: PolarCore, options?: RequestOptions): APIPromise<Result<MetricsLimits, SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=metricsLimits.d.ts.map