import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { MeterQuantities } from "../models/components/meterquantities.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { MetersQuantitiesRequest } from "../models/operations/metersquantities.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Get Meter Quantities
 *
 * @remarks
 * Get quantities of a meter over a time period.
 *
 * **Scopes**: `meters:read` `meters:write`
 */
export declare function metersQuantities(client: PolarCore, request: MetersQuantitiesRequest, options?: RequestOptions): APIPromise<Result<MeterQuantities, ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=metersQuantities.d.ts.map