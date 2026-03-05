import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { MetricsResponse } from "../models/components/metricsresponse.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { MetricsGetRequest } from "../models/operations/metricsget.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
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
export declare function metricsGet(client: PolarCore, request: MetricsGetRequest, options?: RequestOptions): APIPromise<Result<MetricsResponse, HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=metricsGet.d.ts.map