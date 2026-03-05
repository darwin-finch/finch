import { PolarCore } from "../core.js";
import { RequestOptions } from "../lib/sdks.js";
import { Repository } from "../models/components/repository.js";
import { ConnectionError, InvalidRequestError, RequestAbortedError, RequestTimeoutError, UnexpectedClientError } from "../models/errors/httpclienterrors.js";
import { HTTPValidationError } from "../models/errors/httpvalidationerror.js";
import { ResourceNotFound } from "../models/errors/resourcenotfound.js";
import { SDKError } from "../models/errors/sdkerror.js";
import { SDKValidationError } from "../models/errors/sdkvalidationerror.js";
import { RepositoriesGetRequest } from "../models/operations/repositoriesget.js";
import { APIPromise } from "../types/async.js";
import { Result } from "../types/fp.js";
/**
 * Get Repository
 *
 * @remarks
 * Get a repository by ID.
 *
 * **Scopes**: `repositories:read` `repositories:write`
 */
export declare function repositoriesGet(client: PolarCore, request: RepositoriesGetRequest, options?: RequestOptions): APIPromise<Result<Repository, ResourceNotFound | HTTPValidationError | SDKError | SDKValidationError | UnexpectedClientError | InvalidRequestError | RequestAbortedError | RequestTimeoutError | ConnectionError>>;
//# sourceMappingURL=repositoriesGet.d.ts.map